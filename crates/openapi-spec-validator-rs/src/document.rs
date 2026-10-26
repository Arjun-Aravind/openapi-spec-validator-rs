//! Spec documents and `$ref` resolution, mirroring `jsonschema-path`'s `SchemaPath`.
//!
//! Upstream traverses the spec through `SchemaPath`, which transparently follows
//! `$ref` at every step: a mapping whose `$ref` is a string is replaced by its target
//! (sibling keywords are ignored). Resolution uses `referencing`: `#...` fragments
//! are JSON pointers into the current document, anything else is joined with the
//! current document's URI and retrieved.
//!
//! External documents are retrieved when a `$ref` to them is first followed, as
//! upstream's registry does, and kept for the rest of the validation, so the walk can
//! hand out plain references with stable addresses. A node's address is its identity,
//! like Python's `id()`, which upstream uses to deduplicate schema visits.

use std::cell::{RefCell, UnsafeCell};
use std::collections::HashMap;

use percent_encoding::percent_decode_str;
use serde_json::Value;

use crate::loader;
use crate::pyobj;
use crate::pyrepr;
use crate::pyurl;
use crate::retrieve::Retrieve;
use crate::version::python_type_name;

/// A non-validation failure that upstream surfaces as an exception (exit status 2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Crash(pub String);

impl Crash {
    pub(crate) fn recursion() -> Self {
        Self("maximum recursion depth exceeded".to_owned())
    }

    /// `str(KeyError(key))`.
    pub(crate) fn key_error(key: &str) -> Self {
        Self(pyrepr::repr_str(key))
    }
}

/// A failed `Resolver.lookup`: one of `referencing`'s `Unresolvable` exceptions (named by
/// `class`), or another exception raised while following the pointer (`class` is `None`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LookupError {
    class: Option<&'static str>,
    message: String,
}

impl LookupError {
    fn referencing(class: &'static str, message: String) -> Self {
        Self {
            class: Some(class),
            message,
        }
    }

    /// Whether `referencing` raised it (jsonschema wraps those; others propagate as is).
    pub(crate) fn is_referencing(&self) -> bool {
        self.class.is_some()
    }

    /// The exception jsonschema raises while validating: `_WrappedReferencingError`
    /// prints the wrapped class name first.
    pub(crate) fn wrapped(self) -> Crash {
        match self.class {
            Some(class) => Crash(format!("{class}: {}", self.message)),
            None => Crash(self.message),
        }
    }
}

/// The exception jsonschema-path lets through while walking the spec: the bare message.
impl From<LookupError> for Crash {
    fn from(error: LookupError) -> Self {
        Crash(error.message)
    }
}

impl From<Crash> for LookupError {
    fn from(Crash(message): Crash) -> Self {
        Self {
            class: None,
            message,
        }
    }
}

/// The longest `$ref` chain upstream follows before Python's recursion limit (1000
/// frames) stops it, for a chain starting at a component (measured).
const MAX_REF_DEPTH: usize = 977;

/// A resolved position in one of the documents.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Node<'d> {
    pub value: &'d Value,
    /// Index into [`Documents`]: 0 is the root document.
    pub doc: usize,
}

impl<'d> Node<'d> {
    /// Identity of the underlying value, the equivalent of Python's `id()`.
    pub fn id(&self) -> usize {
        std::ptr::from_ref(self.value) as usize
    }
}

struct External {
    uri: String,
    /// The parsed document, or why it could not be retrieved (diagnostic only: upstream
    /// reports every retrieval failure as `Unresolvable(ref)`).
    value: Result<Value, String>,
}

/// The root document plus the external documents its `$ref`s have led to so far.
pub(crate) struct Documents<'a> {
    root: &'a Value,
    root_uri: String,
    /// Fetches `http(s)://` documents; `None` leaves such references unresolvable.
    retriever: Option<&'a dyn Retrieve>,
    /// External documents in the order they were first needed. Failures are kept too:
    /// upstream never retries, as an unresolvable reference ends the validation.
    externals: Arena<External>,
    /// Document index by URI (without fragment): 0 is the root, `n` is `externals[n - 1]`.
    by_uri: RefCell<HashMap<String, usize>>,
    /// The one-character strings that pointers into strings resolve to: Python indexes
    /// a `str` like a sequence and yields a new `str` each time.
    characters: Arena<Value>,
}

impl<'a> Documents<'a> {
    /// Wraps `root`, whose location is `base_uri` (a `file://` or `http(s)://` URI, or
    /// empty). External documents are read or fetched (through `retriever`) when a
    /// `$ref` to them is first resolved.
    pub fn new(root: &'a Value, base_uri: &str, retriever: Option<&'a dyn Retrieve>) -> Self {
        let root_uri = strip_fragment(base_uri).to_owned();
        let by_uri = HashMap::from([(root_uri.clone(), 0)]);
        Self {
            root,
            root_uri,
            retriever,
            externals: Arena::new(),
            by_uri: RefCell::new(by_uri),
            characters: Arena::new(),
        }
    }

    pub fn root(&self) -> Node<'_> {
        Node {
            value: self.root,
            doc: 0,
        }
    }

    /// The URI of document `doc`.
    pub fn uri(&self, doc: usize) -> &str {
        if doc == 0 {
            &self.root_uri
        } else {
            &self.externals.get(doc - 1).uri
        }
    }

    /// `Resolver.lookup(ref)` without following `$ref`s at the target, as
    /// jsonschema's `$ref` keyword does before descending into it.
    pub fn lookup_node(&self, doc: usize, reference: &str) -> Result<Node<'_>, LookupError> {
        self.lookup(doc, reference)
    }

    /// `Resolver.lookup(ref)` for a resolver whose base URI is `base`, which `$id`s met
    /// while validating may have moved away from every document's URI.
    pub fn lookup_from(&self, base: &str, reference: &str) -> Result<Node<'_>, LookupError> {
        let unresolvable = || LookupError::referencing("Unresolvable", reference.to_owned());
        let (uri, fragment) = match reference.strip_prefix('#') {
            Some(fragment) => (base.to_owned(), fragment.to_owned()),
            // `urldefrag(urljoin(base, ref))`; a `ValueError` from either propagates.
            None => {
                let joined = pyurl::urljoin(base, reference).map_err(Crash)?;
                pyurl::urldefrag(&joined).map_err(Crash)?
            }
        };
        let doc = self.document_index(&uri);
        // A document that could not be read or parsed is `Unretrievable`, which
        // `referencing` reports as `Unresolvable(ref)`.
        let contents = self.document(doc).ok_or_else(unresolvable)?;
        self.fragment(doc, contents, &fragment)
    }

    /// The URI documents are known by, as `$id`s and `$ref`s are joined with it
    /// (`urljoin`; a reference `urljoin` rejects is kept as written).
    pub fn join(base: &str, reference: &str) -> String {
        pyurl::urljoin(base, reference).unwrap_or_else(|_| reference.to_owned())
    }

    fn document(&self, doc: usize) -> Option<&Value> {
        if doc == 0 {
            return Some(self.root);
        }
        self.externals.get(doc - 1).value.as_ref().ok()
    }

    /// The index of the document at `uri`, retrieving it on first use like
    /// `Registry.get_or_retrieve`.
    fn document_index(&self, uri: &str) -> usize {
        if let Some(&doc) = self.by_uri.borrow().get(uri) {
            return doc;
        }
        // Retrieval runs a `Retrieve` implementation; no borrow is held meanwhile.
        let value = self.retrieve(uri);
        let doc = self.externals.push(External {
            uri: uri.to_owned(),
            value,
        }) + 1;
        self.by_uri.borrow_mut().insert(uri.to_owned(), doc);
        doc
    }

    /// Reads or fetches the document at `uri` the way upstream's default handlers do.
    fn retrieve(&self, uri: &str) -> Result<Value, String> {
        match pyurl::scheme(uri).as_str() {
            "file" => {
                let path = local_file(uri).ok_or_else(|| format!("not a local file: {uri}"))?;
                let bytes = crate::read_file(&path).map_err(|error| error.to_string())?;
                // Upstream reads files through `urlopen`, which is the stream name
                // PyYAML reports.
                loader::load(&bytes, "<urllib response>").map_err(|error| error.message)
            }
            "http" | "https" => {
                let retriever = self
                    .retriever
                    .ok_or_else(|| "remote references are not retrieved".to_owned())?;
                // The URL goes out exactly as upstream would send it: joined by Python's
                // `urljoin`, never normalised or percent-encoded here.
                let bytes = retriever.retrieve(uri).map_err(|error| error.message)?;
                // An HTTP response has no name for PyYAML to report.
                loader::load(&bytes, "<file>").map_err(|error| error.message)
            }
            // jsonschema-path's handlers accept nothing else.
            scheme => Err(format!("Scheme {scheme} not allowed")),
        }
    }

    /// Follows `$ref`s until reaching a non-reference node (`SchemaNode._resolve_node`).
    pub fn resolve<'d>(&'d self, mut node: Node<'d>) -> Result<Node<'d>, Crash> {
        crate::context::check();
        let mut depth = 0;
        while let Some(reference) = as_ref(node.value) {
            depth += 1;
            if depth > MAX_REF_DEPTH {
                return Err(Crash::recursion());
            }
            node = self.lookup(node.doc, reference)?;
        }
        Ok(node)
    }

    /// `Resolver.lookup(ref)` from `referencing`, relative to document `doc`.
    fn lookup<'d>(&'d self, doc: usize, reference: &str) -> Result<Node<'d>, LookupError> {
        self.lookup_from(self.uri(doc), reference)
    }

    /// The part of `lookup` after retrieval: `fragment` (without `#`) inside `contents`.
    fn fragment<'d>(
        &'d self,
        doc: usize,
        contents: &'d Value,
        fragment: &str,
    ) -> Result<Node<'d>, LookupError> {
        if fragment.is_empty() {
            return Ok(Node {
                value: contents,
                doc,
            });
        }
        if !fragment.starts_with('/') {
            // Anchors (`#foo`) are only found via `$id`/`$anchor` resources, which
            // upstream's registry would have to crawl; none exist in a plain spec.
            if fragment.contains('/') {
                return Err(LookupError::referencing(
                    "InvalidAnchor",
                    format!(
                        "'#{fragment}' is not a valid anchor, neither as a plain name anchor nor \
                         as a JSON Pointer. You may have intended to use '#/{fragment}', as the \
                         slash is required *before each segment* of a JSON pointer."
                    ),
                ));
            }
            return Err(LookupError::referencing(
                "NoSuchAnchor",
                format!(
                    "{} does not exist within {}",
                    pyrepr::repr_str(fragment),
                    pyrepr::repr(contents)
                ),
            ));
        }
        let value = self.pointer(contents, fragment)?;
        Ok(Node { value, doc })
    }

    /// `Resource.pointer` from `referencing`: each segment indexes a mapping by key or a
    /// sequence by position. A Python `str` is a sequence too, so a pointer can reach
    /// into a string and come out with one of its characters.
    fn pointer<'d>(&'d self, document: &'d Value, pointer: &str) -> Result<&'d Value, LookupError> {
        let nowhere = || {
            let mut message = format!(
                "{} does not exist within {}",
                pyrepr::repr_str(pointer),
                pyrepr::repr(document)
            );
            if pointer == "/" {
                message.push_str(
                    ". The pointer '/' is a valid JSON Pointer but it points to an empty string \
                     property ''. If you intended to point to the entire resource, you should use '#'.",
                );
            }
            LookupError::referencing("PointerToNowhere", message)
        };
        let invalid_index = |segment: &str| {
            LookupError::from(Crash(format!(
                "invalid literal for int() with base 10: {}",
                pyrepr::repr_str(segment)
            )))
        };
        // `int(segment)`, then Python's negative indexing.
        let position = |segment: &str, len: usize| -> Result<Option<usize>, LookupError> {
            let index = python_int(segment).ok_or_else(|| invalid_index(segment))?;
            let index = if index < 0 {
                i128::try_from(len).unwrap_or(i128::MAX) + index
            } else {
                index
            };
            Ok(usize::try_from(index).ok().filter(|&i| i < len))
        };
        let decoded = percent_decode_str(&pointer[1..]).decode_utf8_lossy();
        let mut contents = document;
        for segment in decoded.split('/') {
            contents = match contents {
                Value::Array(items) => position(segment, items.len())?
                    .map(|i| &items[i])
                    .ok_or_else(nowhere)?,
                Value::Object(map) if pyrepr::foreign(contents).is_none() => {
                    let key = segment.replace("~1", "/").replace("~0", "~");
                    map.get(&key).ok_or_else(nowhere)?
                }
                Value::String(text) => {
                    let character = position(segment, text.chars().count())?
                        .and_then(|i| text.chars().nth(i))
                        .ok_or_else(nowhere)?;
                    let index = self.characters.push(Value::String(character.to_string()));
                    self.characters.get(index)
                }
                other => {
                    return Err(Crash(format!(
                        "'{}' object is not subscriptable",
                        python_type_name(other)
                    ))
                    .into());
                }
            };
        }
        Ok(contents)
    }

    /// The child `key` of a mapping node, resolved (`path / key`).
    pub fn get<'d>(&'d self, node: Node<'d>, key: &str) -> Result<Option<Node<'d>>, Crash> {
        match pyobj::as_dict(node.value) {
            Some(map) => match map.get(key) {
                Some(value) => self
                    .resolve(Node {
                        value,
                        doc: node.doc,
                    })
                    .map(Some),
                None => Ok(None),
            },
            _ => Ok(None),
        }
    }

    /// Like [`Documents::get`] for a key known to exist (checked with `contains`).
    pub fn child<'d>(&'d self, node: Node<'d>, key: &str) -> Result<Node<'d>, Crash> {
        self.get(node, key)?.ok_or_else(|| Crash::key_error(key))
    }

    /// The children of a node, as `for child in path` iterates them: list items or
    /// mapping values, in order. Children are not yet resolved: the caller resolves
    /// each right before use, matching upstream's evaluation order. `name` is the key
    /// the node was reached by, used in the `KeyError` upstream raises for leaves.
    pub fn children<'d>(&'d self, node: Node<'d>, name: &str) -> Result<Vec<Node<'d>>, Crash> {
        Ok(self
            .entries(node, name)?
            .into_iter()
            .map(|(_, child)| child)
            .collect())
    }

    /// `(key, child)` pairs (`path.items()`): mapping keys, or indices for lists.
    /// Children are not yet resolved: resolve them right before use.
    pub fn entries<'d>(
        &'d self,
        node: Node<'d>,
        name: &str,
    ) -> Result<Vec<(Key<'d>, Node<'d>)>, Crash> {
        let doc = node.doc;
        match node.value {
            Value::Object(map) if pyrepr::foreign(node.value).is_none() => Ok(map
                .iter()
                .map(|(key, value)| (Key::Str(key), Node { value, doc }))
                .collect()),
            Value::Array(items) => Ok(items
                .iter()
                .map(|value| (Key::Index, Node { value, doc }))
                .collect()),
            // `keys()` of a leaf raises `KeyError(<last path part>)`.
            _ => Err(Crash::key_error(name)),
        }
    }
}

/// A mapping key or list index produced by [`Documents::entries`].
#[derive(Clone, Copy, Debug)]
pub(crate) enum Key<'d> {
    Str(&'d str),
    Index,
}

/// `is_ref` from `jsonschema-path`: a mapping whose `$ref` is a string.
pub(crate) fn as_ref(value: &Value) -> Option<&str> {
    pyobj::as_dict(value)?.get("$ref")?.as_str()
}

/// Python `int(s)` for base-10 literals (ASCII digits, sign, underscores, whitespace).
fn python_int(s: &str) -> Option<i128> {
    let s = pyobj::decimal_to_ascii(s)?;
    let s = s.trim();
    let (negative, digits) = match s.as_bytes().first()? {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
        || !digits.bytes().all(|b| b.is_ascii_digit() || b == b'_')
    {
        return None;
    }
    let value: i128 = digits.replace('_', "").parse().ok()?;
    Some(if negative { -value } else { value })
}

/// Append-only storage whose elements never move: a reference to an element stays valid
/// for as long as the arena does, even as elements are added through a shared reference.
/// [`Documents`] hands out [`Node`]s into documents retrieved while the walk is under way,
/// as upstream's registry grows while references are resolved.
struct Arena<T> {
    items: UnsafeCell<Vec<Box<T>>>,
}

impl<T> Arena<T> {
    fn new() -> Self {
        Self {
            items: UnsafeCell::new(Vec::new()),
        }
    }

    /// Appends `item` and returns its index.
    fn push(&self, item: T) -> usize {
        // SAFETY: the vector is only accessed here and in `get`, each for the duration
        // of the call and without calling back into the arena, and `UnsafeCell` makes
        // the arena `!Sync`, so no two accesses overlap. Elements are boxed and never
        // removed, so references returned by `get` survive the vector growing.
        let items = unsafe { &mut *self.items.get() };
        items.push(Box::new(item));
        items.len() - 1
    }

    fn get(&self, index: usize) -> &T {
        // SAFETY: see `push`.
        let items = unsafe { &*self.items.get() };
        &items[index]
    }
}

fn strip_fragment(uri: &str) -> &str {
    uri.split_once('#').map_or(uri, |(base, _)| base)
}

/// The file `urlopen` reads for the `file:` URL `uri` (see [`pyurl::file_url_path`]).
#[cfg(unix)]
fn local_file(uri: &str) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let bytes = pyurl::file_url_path(uri, crate::context::python_minor())?;
    Some(std::ffi::OsString::from_vec(bytes).into())
}

/// The file for the `file:` URL `uri`. Python's Windows conversion (`nturl2path`) is not
/// reproduced: the URL is read as a standard file URL, and a query is never part of a
/// file name.
#[cfg(not(unix))]
fn local_file(uri: &str) -> Option<std::path::PathBuf> {
    let url = url::Url::parse(uri).ok()?;
    if url.query().is_some() && crate::context::python_minor() < 14 {
        return None;
    }
    url.to_file_path().ok()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn resolves_local_refs_transitively() {
        let spec = json!({
            "a": {"$ref": "#/b"},
            "b": {"$ref": "#/c/0"},
            "c": [{"type": "string"}],
        });
        let docs = Documents::new(&spec, "", None);
        let a = docs.child(docs.root(), "a").unwrap();
        assert_eq!(a.value, &json!({"type": "string"}));
        assert_eq!(
            a.id(),
            docs.child(docs.root(), "c").unwrap().value[0]
                .as_object()
                .map_or(0, |_| std::ptr::from_ref(&spec["c"][0]) as usize)
        );
    }

    #[test]
    fn decodes_pointer_escapes() {
        let spec = json!({
            "paths": {"/pets/{id}": {"get": {}}},
            "x": {"$ref": "#/paths/~1pets~1%7Bid%7D/get"},
        });
        let docs = Documents::new(&spec, "", None);
        assert_eq!(docs.child(docs.root(), "x").unwrap().value, &json!({}));
    }

    #[test]
    fn missing_targets_crash_like_upstream() {
        let spec = json!({"a": {"$ref": "#/nope"}});
        let docs = Documents::new(&spec, "", None);
        let Crash(message) = docs.child(docs.root(), "a").unwrap_err();
        assert_eq!(
            message,
            "'/nope' does not exist within {'a': {'$ref': '#/nope'}}"
        );
    }

    #[test]
    fn pointers_index_strings_like_python() {
        let spec = json!({
            "info": {"title": "tx"},
            "a": {"$ref": "#/info/title/0"},
            "b": {"$ref": "#/info/title/-1"},
            "c": {"$ref": "#/info/title/x"},
            "d": {"$ref": "#/info/title/2"},
            "e": {"$ref": "#/info/title/1/0/0"},
        });
        let docs = Documents::new(&spec, "", None);
        assert_eq!(docs.child(docs.root(), "a").unwrap().value, &json!("t"));
        assert_eq!(docs.child(docs.root(), "b").unwrap().value, &json!("x"));
        assert_eq!(docs.child(docs.root(), "e").unwrap().value, &json!("x"));
        assert_eq!(
            docs.child(docs.root(), "c").unwrap_err(),
            Crash("invalid literal for int() with base 10: 'x'".to_owned())
        );
        assert!(
            docs.child(docs.root(), "d")
                .unwrap_err()
                .0
                .starts_with("'/info/title/2' does not exist within")
        );
        // Each resolution yields a new string, as in Python.
        let first = docs.child(docs.root(), "a").unwrap().id();
        assert_ne!(first, docs.child(docs.root(), "a").unwrap().id());
    }

    #[test]
    fn ref_cycles_hit_the_recursion_limit() {
        let spec = json!({"a": {"$ref": "#/b"}, "b": {"$ref": "#/a"}});
        let docs = Documents::new(&spec, "", None);
        assert_eq!(
            docs.child(docs.root(), "a").unwrap_err(),
            Crash::recursion()
        );
    }

    #[test]
    fn non_string_refs_are_plain_mappings() {
        let spec = json!({"a": {"$ref": 5}});
        let docs = Documents::new(&spec, "", None);
        assert_eq!(
            docs.child(docs.root(), "a").unwrap().value,
            &json!({"$ref": 5})
        );
    }

    #[test]
    fn python_int_parsing() {
        assert_eq!(python_int("12"), Some(12));
        assert_eq!(python_int(" -1 "), Some(-1));
        assert_eq!(python_int("1_0"), Some(10));
        assert_eq!(python_int("1__0"), None);
        assert_eq!(python_int("x"), None);
        // Python's `int()` reads every script's decimal digits and whitespace.
        assert_eq!(python_int("\u{660}\u{661}"), Some(1));
        assert_eq!(python_int("\u{3000}-\u{ff11}\u{ff12} "), Some(-12));
        assert_eq!(python_int("\u{660}x"), None);
        assert_eq!(python_int("\u{2160}"), None);
    }
}
