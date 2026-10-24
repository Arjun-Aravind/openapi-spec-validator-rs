//! Spec documents and `$ref` resolution, mirroring `jsonschema-path`'s `SchemaPath`.
//!
//! Upstream traverses the spec through `SchemaPath`, which transparently follows
//! `$ref` at every step: a mapping whose `$ref` is a string is replaced by its target
//! (sibling keywords are ignored). Resolution uses `referencing`: `#...` fragments
//! are JSON pointers into the current document, anything else is joined with the
//! current document's URI and retrieved.
//!
//! External documents are loaded up front (a scan for `$ref`s), so the walk can hand
//! out plain references with stable addresses. A node's address is its identity,
//! like Python's `id()`, which upstream uses to deduplicate schema visits.

use std::collections::HashMap;

use percent_encoding::percent_decode_str;
use serde_json::Value;
use url::Url;

use crate::loader;
use crate::pyrepr;
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
    value: Result<Value, String>,
}

/// The root document plus every external document reachable through `$ref`.
pub(crate) struct Documents<'a> {
    root: &'a Value,
    root_uri: String,
    externals: Vec<External>,
    by_uri: HashMap<String, usize>,
}

impl<'a> Documents<'a> {
    /// Wraps `root`, whose location is `base_uri` (a `file://` URI, or empty), and
    /// loads the external documents its `$ref`s point to.
    pub fn new(root: &'a Value, base_uri: &str) -> Self {
        let mut docs = Self {
            root,
            root_uri: strip_fragment(base_uri).to_owned(),
            externals: Vec::new(),
            by_uri: HashMap::new(),
        };
        docs.by_uri.insert(docs.root_uri.clone(), 0);
        docs.load_externals();
        docs
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
            &self.externals[doc - 1].uri
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
            None => {
                let joined = join_uri(base, reference).ok_or_else(unresolvable)?;
                let (uri, fragment) = joined.split_once('#').unwrap_or((&joined, ""));
                (uri.to_owned(), fragment.to_owned())
            }
        };
        let doc = *self.by_uri.get(&uri).ok_or_else(unresolvable)?;
        // A document that could not be read or parsed is `Unretrievable`, which
        // `referencing` reports as `Unresolvable(ref)`.
        let contents = self.document(doc).ok_or_else(unresolvable)?;
        self.fragment(doc, contents, &fragment)
    }

    /// The URI documents are known by, as `$id`s and `$ref`s are joined with it.
    pub fn join(base: &str, reference: &str) -> String {
        if reference.is_empty() {
            return base.to_owned();
        }
        if base.is_empty() {
            return reference.to_owned();
        }
        join_uri(base, reference).unwrap_or_else(|| reference.to_owned())
    }

    fn document(&self, doc: usize) -> Option<&Value> {
        if doc == 0 {
            return Some(self.root);
        }
        self.externals[doc - 1].value.as_ref().ok()
    }

    fn load_externals(&mut self) {
        let mut pending = vec![0];
        while let Some(doc) = pending.pop() {
            let mut refs = Vec::new();
            if let Some(value) = self.document(doc) {
                collect_refs(value, &mut refs);
            }
            let base = self.uri(doc).to_owned();
            for reference in refs {
                if reference.starts_with('#') {
                    continue;
                }
                let Some(uri) = join_uri(&base, &reference) else {
                    continue;
                };
                let uri = strip_fragment(&uri).to_owned();
                if self.by_uri.contains_key(&uri) {
                    continue;
                }
                let value = retrieve(&uri, &reference);
                self.externals.push(External {
                    uri: uri.clone(),
                    value,
                });
                let index = self.externals.len();
                self.by_uri.insert(uri, index);
                pending.push(index);
            }
        }
    }

    /// Follows `$ref`s until reaching a non-reference node (`SchemaNode._resolve_node`).
    pub fn resolve<'d>(&'d self, mut node: Node<'d>) -> Result<Node<'d>, Crash> {
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
        let value = pointer(contents, fragment)?;
        Ok(Node { value, doc })
    }

    /// The child `key` of a mapping node, resolved (`path / key`).
    pub fn get<'d>(&'d self, node: Node<'d>, key: &str) -> Result<Option<Node<'d>>, Crash> {
        match node.value {
            Value::Object(map) => match map.get(key) {
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
            Value::Object(map) => Ok(map
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
    value.as_object()?.get("$ref")?.as_str()
}

/// `Resource.pointer` from `referencing`.
fn pointer<'d>(document: &'d Value, pointer: &str) -> Result<&'d Value, LookupError> {
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
    let decoded = percent_decode_str(&pointer[1..]).decode_utf8_lossy();
    let mut contents = document;
    for segment in decoded.split('/') {
        contents = match contents {
            Value::Array(items) => {
                let index = python_int(segment).ok_or_else(|| {
                    Crash(format!(
                        "invalid literal for int() with base 10: {}",
                        pyrepr::repr_str(segment)
                    ))
                })?;
                let index = if index < 0 {
                    i128::try_from(items.len()).unwrap_or(i128::MAX) + index
                } else {
                    index
                };
                usize::try_from(index)
                    .ok()
                    .and_then(|i| items.get(i))
                    .ok_or_else(nowhere)?
            }
            Value::Object(map) => {
                let key = segment.replace("~1", "/").replace("~0", "~");
                map.get(&key).ok_or_else(nowhere)?
            }
            // Strings are sequences in Python, so indexing them yields a new
            // one-character string that has no address in the document.
            Value::String(_) => return Err(nowhere()),
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

/// Python `int(s)` for base-10 literals (ASCII digits, sign, underscores, whitespace).
fn python_int(s: &str) -> Option<i128> {
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

/// Every string that may be resolved as a reference: `$ref`s, and discriminator mapping
/// values, which OpenAPI 3.0 default validation follows.
fn collect_refs(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(reference)) = map.get("$ref") {
                out.push(reference.clone());
            }
            if let Some(Value::Object(mapping)) = map
                .get("discriminator")
                .and_then(|discriminator| discriminator.get("mapping"))
            {
                out.extend(
                    mapping
                        .values()
                        .filter_map(Value::as_str)
                        .map(str::to_owned),
                );
            }
            for child in map.values() {
                collect_refs(child, out);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_refs(child, out);
            }
        }
        _ => {}
    }
}

fn strip_fragment(uri: &str) -> &str {
    uri.split_once('#').map_or(uri, |(base, _)| base)
}

/// `urljoin(base, reference)`; `None` when the result has no usable scheme.
fn join_uri(base: &str, reference: &str) -> Option<String> {
    if let Ok(absolute) = Url::parse(reference) {
        return Some(absolute.to_string());
    }
    Url::parse(base)
        .ok()?
        .join(reference)
        .ok()
        .map(|url| url.to_string())
}

/// Retrieves an external document the way upstream's default handlers would.
fn retrieve(uri: &str, reference: &str) -> Result<Value, String> {
    let unresolvable = || reference.to_owned();
    let url = Url::parse(uri).map_err(|_| unresolvable())?;
    if url.scheme() != "file" {
        // Upstream fetches http(s) references over the network; this port does not.
        return Err(unresolvable());
    }
    // urllib opens the URL's whole selector, query included, as the file name.
    if url.query().is_some() {
        return Err(unresolvable());
    }
    let path = url.to_file_path().map_err(|()| unresolvable())?;
    let bytes = std::fs::read(&path).map_err(|_| unresolvable())?;
    loader::load(&bytes, "<urllib response>").map_err(|error| error.message)
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
        let docs = Documents::new(&spec, "");
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
        let docs = Documents::new(&spec, "");
        assert_eq!(docs.child(docs.root(), "x").unwrap().value, &json!({}));
    }

    #[test]
    fn missing_targets_crash_like_upstream() {
        let spec = json!({"a": {"$ref": "#/nope"}});
        let docs = Documents::new(&spec, "");
        let Crash(message) = docs.child(docs.root(), "a").unwrap_err();
        assert_eq!(
            message,
            "'/nope' does not exist within {'a': {'$ref': '#/nope'}}"
        );
    }

    #[test]
    fn ref_cycles_hit_the_recursion_limit() {
        let spec = json!({"a": {"$ref": "#/b"}, "b": {"$ref": "#/a"}});
        let docs = Documents::new(&spec, "");
        assert_eq!(
            docs.child(docs.root(), "a").unwrap_err(),
            Crash::recursion()
        );
    }

    #[test]
    fn non_string_refs_are_plain_mappings() {
        let spec = json!({"a": {"$ref": 5}});
        let docs = Documents::new(&spec, "");
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
    }
}
