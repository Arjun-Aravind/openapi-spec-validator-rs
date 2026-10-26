//! Python's recursion limit where upstream recurses as deep as a document nests.
//!
//! jsonschema validates by recursion: every subschema it enters costs a `descend` frame
//! and a keyword frame, so checking a document against its meta-schema, or a schema
//! against its dialect's meta-schema, uses Python stack in proportion to how deeply the
//! instance nests through the meta-schema. Past the interpreter's recursion limit
//! (1000) upstream raises `RecursionError` and reports `maximum recursion depth
//! exceeded`; the `jsonschema` crate has no such limit, and the error trees of deeply
//! nested invalid documents grow quadratically.
//!
//! [`Probe`] replays jsonschema's traversal of an instance through a meta-schema and
//! counts the frames upstream's call chain would hold at every call, including the
//! short-lived chains each step makes (reference lookup, `evolve`, type checks). The
//! counts were fitted to measurements of upstream (`scripts/frames/`). It reports the
//! first point, in Python's evaluation order, where the limit would be exceeded, and the
//! instance locations where it is, so that the rest of a document can still be checked
//! for an error upstream would report first.

use serde_json::{Map, Value};

use crate::meta::Schemas;
use crate::pyobj;
use crate::pyregex::PyRegex;
use crate::pyrepr::PathItem;
use crate::schema_path::Search;

/// Python's default recursion limit.
pub(crate) const DEFAULT_LIMIT: usize = 1000;

/// Frames below the document check's `iter_errors` when upstream runs from its console
/// script (`openapi-spec-validator`): `<module>`, `main`, `validate` (shortcut),
/// `validate` (validator), two cache iterators, the decorator's wrapper and
/// `SpecValidator.iter_errors`.
pub(crate) const CLI_SPEC_DEPTH: usize = 8;

/// Where to stop: the depth of the frame that iterates the meta-schema's root keywords
/// (`iter_errors`), and the recursion limit.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Budget {
    pub root: usize,
    pub limit: usize,
}

impl Budget {
    fn available(self) -> usize {
        self.limit.saturating_sub(self.root)
    }
}

/// Where the limit is first exceeded.
#[derive(Debug, Clone)]
pub(crate) struct Crossing {
    /// The Python schema path (from the meta-schema root, `$ref` hops left out).
    pub schema_path: Vec<PathItem>,
    /// The instance path.
    pub instance_path: Vec<PathItem>,
}

/// How the keywords of a subschema are reached; it decides which short-lived calls
/// the frame that iterates them makes first.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Entry {
    /// The validator's own `iter_errors`.
    Root,
    /// `descend`, possibly through a reference.
    Descend,
    /// `is_valid` after `evolve` (its `iter_errors`).
    IsValid,
}

pub(crate) struct Probe<'a> {
    search: Search<'a>,
    /// Draft 4 (`$ref` hides its siblings, legacy `items` and `dependencies`) rather
    /// than 2020-12.
    draft4: bool,
    available: usize,
    /// The deepest point reached, relative to the root (`iter_errors` is 0).
    pub max: usize,
    /// Where it was reached (schema path, instance path), when `track_max_at` is set:
    /// on a deep document a copy at every new maximum costs as much as the rest.
    pub max_at: (Vec<PathItem>, Vec<PathItem>),
    pub track_max_at: bool,
    pub crossing: Option<Crossing>,
    /// The outermost instance locations where the limit is exceeded.
    pub prune: Vec<Vec<PathItem>>,
    schema_path: Vec<PathItem>,
    instance_path: Vec<PathItem>,
    regexes: std::collections::HashMap<String, Option<PyRegex>>,
    /// Whether formats are checked (the dialect check; not the document check).
    formats: bool,
    /// Whether a dynamic reference was looked up yet: the first crawls the registry.
    crawled: bool,
    /// Inside an `is_valid` check: Python stops at the first error, so a keyword that
    /// evidently fails ends the check.
    lazy: bool,
    /// Whether a keyword evidently failed in the evaluation under way (see
    /// [`Probe::evidently_fails`]): it decides where the evaluation is lazy.
    failed: bool,
}

/// Patterns Python's `re` module has compiled in this process, as far as the model
/// knows: the first `re.search` of a pattern parses it, several frames deeper.
static COMPILED: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(Default::default);

/// Frames `re.search(pattern, ...)` or `re.compile(pattern)` uses above its caller:
/// `_compile`'s cache lookup, or on first use the parser (see [`parse_depth`]).
fn regex_cost(pattern: &str) -> usize {
    let mut compiled = COMPILED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if compiled.contains(pattern) {
        return 2;
    }
    compiled.insert(pattern.to_owned());
    // `search`/`compile`, `_compile`, `compile`, `parse`, `_parse_sub`, `_parse`.
    6 + parse_depth(pattern)
}

/// The deepest chain `sre_parse._parse` calls, two frames per group level: `get` and
/// `__next` for a plain token, one more for an escape that reads digits
/// (`_escape`/`_class_escape`, `getwhile`) or fails (`error`), or inline flags.
fn parse_depth(pattern: &str) -> usize {
    let chars: Vec<char> = pattern.chars().collect();
    let (mut level, mut deepest, mut class) = (0usize, 2usize, false);
    let mut i = 0;
    while i < chars.len() {
        let here = 2 * level;
        match chars[i] {
            '\\' => {
                let next = chars.get(i + 1).copied().unwrap_or('\0');
                let costly = match next {
                    'x' | 'u' | 'U' | 'N' | '0'..='9' => true,
                    c if c.is_ascii_alphabetic() => {
                        let valid = if class {
                            "bdDsSwWafnrtv"
                        } else {
                            "AbBdDsSwWZafnrtv"
                        };
                        !valid.contains(c)
                    }
                    _ => false,
                };
                deepest = deepest.max(here + 2 + usize::from(costly));
                i += 2;
                continue;
            }
            '[' if !class => class = true,
            ']' if class => class = false,
            '(' if !class => {
                level += 1;
                let flags = chars.get(i + 1) == Some(&'?')
                    && chars
                        .get(i + 2)
                        .is_some_and(|c| c.is_ascii_alphabetic() || *c == '-');
                if flags {
                    deepest = deepest.max(here + 3);
                }
            }
            ')' if !class => level = level.saturating_sub(1),
            _ => {}
        }
        deepest = deepest.max(2 * level + 2);
        i += 1;
    }
    deepest
}

/// The target `Search` is constructed with; the probe only uses it to resolve references.
static NULL: Value = Value::Null;

impl<'a> Probe<'a> {
    pub(crate) fn new(schemas: &'a Schemas<'a>, budget: Budget, formats: bool) -> Self {
        Self {
            search: Search::new(&schemas.resources, schemas.ref_overrides, &[], &NULL, ""),
            draft4: schemas.ref_overrides,
            available: budget.available(),
            max: 0,
            max_at: (Vec::new(), Vec::new()),
            track_max_at: false,
            crossing: None,
            prune: Vec::new(),
            schema_path: Vec::new(),
            instance_path: Vec::new(),
            regexes: std::collections::HashMap::new(),
            formats,
            crawled: false,
            lazy: false,
            failed: false,
        }
    }

    /// Replays the validation of `instance` against the first resource.
    pub(crate) fn run(&mut self, instance: &Value) {
        let Some(root) = self.search.root() else {
            return;
        };
        self.visit(instance, root.contents, root.uri, 0, Entry::Root);
    }

    /// Records a call reaching `depth` frames above the root; false once that exceeds
    /// the limit (the caller goes no further).
    fn reach(&mut self, depth: usize) -> bool {
        if depth > self.max {
            self.max = depth;
            if self.track_max_at {
                self.max_at = (self.schema_path.clone(), self.instance_path.clone());
            }
        }
        if depth <= self.available {
            return true;
        }
        if self.crossing.is_none() {
            self.crossing = Some(Crossing {
                schema_path: self.schema_path.clone(),
                instance_path: self.instance_path.clone(),
            });
        }
        if !self
            .prune
            .iter()
            .any(|outer| self.instance_path.starts_with(outer))
        {
            self.prune
                .retain(|inner| !inner.starts_with(&self.instance_path));
            self.prune.push(self.instance_path.clone());
        }
        false
    }

    /// The keywords of `schema` applied to `instance`, iterated by a frame at `depth`.
    fn visit(
        &mut self,
        instance: &Value,
        schema: &'a Value,
        base: &'a str,
        depth: usize,
        entry: Entry,
    ) {
        crate::context::check();
        let Value::Object(map) = schema else {
            return;
        };
        // What the iterating frame calls before the keywords: `descend` evolves the
        // validator (and enters the subresource); `validator_for` normalises the URI of
        // a `$schema` it finds.
        let setup = match entry {
            Entry::Root | Entry::IsValid => 0,
            Entry::Descend if map.contains_key("$schema") => 8,
            Entry::Descend if self.draft4 => 4,
            Entry::Descend => 3,
        };
        if !self.reach(depth + setup) {
            return;
        }
        if self.draft4 {
            if let Some(reference) = map.get("$ref") {
                self.keyword(instance, map, "$ref", reference, base, depth + 1);
                return;
            }
        }
        for (keyword, value) in map {
            if (self.lazy && self.failed) || !self.reach(depth + 1) {
                return;
            }
            self.keyword(instance, map, keyword, value, base, depth + 1);
            if self.evidently_fails(keyword, value, instance) {
                self.failed = true;
            }
        }
    }

    /// Runs `f` as an evaluation of its own (an `is_valid` check when `lazy`), whether
    /// it evidently failed.
    fn scoped(&mut self, lazy: bool, f: impl FnOnce(&mut Self)) -> bool {
        let outer = (self.lazy, self.failed);
        self.lazy = lazy;
        self.failed = false;
        f(self);
        let failed = self.failed;
        (self.lazy, self.failed) = outer;
        failed
    }

    /// Whether `keyword` fails on `instance` on its own, as far as a cheap check tells.
    fn evidently_fails(&self, keyword: &str, value: &Value, instance: &Value) -> bool {
        match keyword {
            "type" => {
                let types: Vec<&str> = match value {
                    Value::String(t) => vec![t.as_str()],
                    Value::Array(items) => items.iter().filter_map(Value::as_str).collect(),
                    _ => return false,
                };
                !types.is_empty()
                    && types.iter().all(|t| {
                        crate::meta::is_python_type(instance, t, self.draft4) == Some(false)
                    })
            }
            "required" => pyobj::as_dict(instance).is_some_and(|object| {
                value.as_array().is_some_and(|names| {
                    names
                        .iter()
                        .any(|n| n.as_str().is_some_and(|n| !object.contains_key(n)))
                })
            }),
            "enum" => value
                .as_array()
                .is_some_and(|options| !options.iter().any(|o| pyobj::json_equal(o, instance))),
            "const" => !pyobj::json_equal(value, instance),
            _ => false,
        }
    }

    /// The keyword function `keyword`, its frame at `depth`.
    #[allow(clippy::too_many_lines)]
    fn keyword(
        &mut self,
        instance: &Value,
        schema: &'a Map<String, Value>,
        keyword: &'a str,
        value: &'a Value,
        base: &'a str,
        depth: usize,
    ) {
        let draft4 = self.draft4;
        // The deepest short-lived chain the keyword itself calls: `type`'s generator
        // and type checker, the `is_type(instance, "object")` guards, `enum`'s `equal`.
        let own = match keyword {
            "type" => 4,
            "enum" | "const" => 2,
            "properties"
            | "patternProperties"
            | "additionalProperties"
            | "required"
            | "minProperties"
            | "maxProperties"
            | "propertyNames"
            | "dependencies"
            | "dependentRequired"
            | "dependentSchemas"
            | "unevaluatedProperties"
            | "items"
            | "prefixItems"
            | "additionalItems"
            | "contains"
            | "minItems"
            | "maxItems"
            | "uniqueItems"
            | "unevaluatedItems"
            | "minLength"
            | "maxLength"
            | "pattern"
            | "minimum"
            | "maximum"
            | "exclusiveMinimum"
            | "exclusiveMaximum"
            | "multipleOf" => 3,
            "allOf" | "anyOf" => 1,
            "oneOf" | "not" | "if" => 4,
            _ => 0,
        };
        if !self.reach(depth + own) {
            return;
        }
        let object = pyobj::as_dict(instance);
        // Regular expressions: the keyword's `re.search`, `find_additional_properties`'
        // search of the joined patterns, and the `regex` format's `re.compile`.
        let regex = match (keyword, value) {
            ("pattern", Value::String(pattern)) if instance.is_string() => {
                Some((0, pattern.clone()))
            }
            ("patternProperties", Value::Object(patterns))
                if object.is_some_and(|o| !o.is_empty()) =>
            {
                patterns
                    .keys()
                    .map(|p| regex_cost(p))
                    .max()
                    .map(|cost| (cost, String::new()))
            }
            ("additionalProperties", _) => {
                let patterns: Vec<&str> = schema
                    .get("patternProperties")
                    .and_then(Value::as_object)
                    .map(|p| p.keys().map(String::as_str).collect())
                    .unwrap_or_default();
                let names = schema.get("properties").and_then(Value::as_object);
                let searched = object
                    .is_some_and(|o| o.keys().any(|k| !names.is_some_and(|n| n.contains_key(k))));
                (!patterns.is_empty() && searched)
                    .then(|| (1 + regex_cost(&patterns.join("|")), String::new()))
            }
            ("format", Value::String(format)) if self.formats && format == "regex" => instance
                .as_str()
                .map(|pattern| (3 + regex_cost(pattern), String::new())),
            _ => None,
        };
        if let Some((cost, pattern)) = regex {
            let cost = if pattern.is_empty() {
                cost
            } else {
                regex_cost(&pattern)
            };
            if !self.reach(depth + cost) {
                return;
            }
        }
        match keyword {
            "$ref" | "$dynamicRef" => {
                let Value::String(reference) = value else {
                    return;
                };
                // `_validate_reference` and the lookup's deepest call: a pointer into a
                // resource goes through `in_subresource`, a dynamic reference through
                // the dynamic scope.
                let lookup = if keyword == "$dynamicRef" {
                    let first = !std::mem::replace(&mut self.crawled, true);
                    8 + usize::from(first)
                } else if reference.contains("#/") {
                    7
                } else {
                    6
                };
                if !self.reach(depth + lookup) {
                    return;
                }
                let target = if keyword == "$dynamicRef" {
                    self.search.resolve_dynamic(base, reference)
                } else {
                    self.search.resolve(base, reference)
                };
                if let Some((target, target_base)) = target {
                    let keep = keyword == "$dynamicRef";
                    self.descend(
                        instance,
                        target,
                        target_base,
                        depth,
                        keep.then_some(keyword),
                        None,
                    );
                }
            }
            "properties" => {
                let (Some(object), Value::Object(properties)) = (object, value) else {
                    return;
                };
                for (name, subschema) in properties {
                    if let Some(member) = object.get(name) {
                        self.with_paths(
                            Some(keyword),
                            Some(PathItem::Key(name.clone())),
                            Some(PathItem::Key(name.clone())),
                            |probe| {
                                probe.visit(member, subschema, base, depth + 1, Entry::Descend);
                            },
                        );
                    }
                }
            }
            "patternProperties" => {
                let (Some(object), Value::Object(patterns)) = (object, value) else {
                    return;
                };
                for (pattern, subschema) in patterns {
                    for (name, member) in object {
                        if self.search_pattern(pattern, name) {
                            self.with_paths(
                                Some(keyword),
                                Some(PathItem::Key(pattern.clone())),
                                Some(PathItem::Key(name.clone())),
                                |probe| {
                                    probe.visit(member, subschema, base, depth + 1, Entry::Descend);
                                },
                            );
                        }
                    }
                }
            }
            "additionalProperties" => {
                let Some(object) = object else { return };
                if !value.is_object() {
                    return;
                }
                let properties = schema.get("properties").and_then(Value::as_object);
                let patterns: Vec<&String> = schema
                    .get("patternProperties")
                    .and_then(Value::as_object)
                    .map(|p| p.keys().collect())
                    .unwrap_or_default();
                let joined = patterns
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join("|");
                for (name, member) in object {
                    if properties.is_some_and(|p| p.contains_key(name)) {
                        continue;
                    }
                    if !joined.is_empty() && self.search_pattern(&joined, name) {
                        continue;
                    }
                    self.with_paths(
                        Some(keyword),
                        None,
                        Some(PathItem::Key(name.clone())),
                        |probe| {
                            probe.visit(member, value, base, depth + 1, Entry::Descend);
                        },
                    );
                }
            }
            "items" | "prefixItems" | "additionalItems" => {
                let Value::Array(items) = instance else {
                    return;
                };
                match (keyword, value) {
                    ("items", Value::Array(subschemas))
                    | ("prefixItems", Value::Array(subschemas)) => {
                        for (index, (item, subschema)) in items.iter().zip(subschemas).enumerate() {
                            self.with_paths(
                                Some(keyword),
                                Some(PathItem::Index(index)),
                                Some(PathItem::Index(index)),
                                |probe| {
                                    probe.visit(item, subschema, base, depth + 1, Entry::Descend);
                                },
                            );
                        }
                    }
                    ("items", Value::Object(_)) => {
                        let prefix = if draft4 {
                            0
                        } else {
                            schema
                                .get("prefixItems")
                                .and_then(Value::as_array)
                                .map_or(0, Vec::len)
                        };
                        for (index, item) in items.iter().enumerate().skip(prefix) {
                            self.with_paths(
                                Some(keyword),
                                None,
                                Some(PathItem::Index(index)),
                                |probe| {
                                    probe.visit(item, value, base, depth + 1, Entry::Descend);
                                },
                            );
                        }
                    }
                    ("additionalItems", Value::Object(_)) => {
                        let Some(Value::Array(prefix)) = schema.get("items") else {
                            return;
                        };
                        for (index, item) in items.iter().enumerate().skip(prefix.len()) {
                            self.with_paths(
                                Some(keyword),
                                None,
                                Some(PathItem::Index(index)),
                                |probe| {
                                    probe.visit(item, value, base, depth + 1, Entry::Descend);
                                },
                            );
                        }
                    }
                    _ => {}
                }
            }
            "allOf" | "anyOf" | "oneOf" => {
                let Value::Array(subschemas) = value else {
                    return;
                };
                if keyword == "allOf" {
                    for (index, subschema) in subschemas.iter().enumerate() {
                        self.with_paths(
                            Some(keyword),
                            Some(PathItem::Index(index)),
                            None,
                            |probe| {
                                probe.visit(instance, subschema, base, depth + 1, Entry::Descend);
                            },
                        );
                    }
                    return;
                }
                // Branches are evaluated in full until one holds; `anyOf` stops there,
                // `oneOf` checks the rest with `is_valid`, which stops at a first error.
                let mut held = false;
                let mut all_failed = true;
                for (index, subschema) in subschemas.iter().enumerate() {
                    let lazy = keyword == "oneOf" && held;
                    let failed = self.scoped(lazy, |probe| {
                        probe.with_paths(
                            Some(keyword),
                            Some(PathItem::Index(index)),
                            None,
                            |probe| {
                                // Upstream checks a branch after one that held with
                                // `is_valid`, a frame deeper; whether one held is only
                                // guessed here, and `descend`'s depth matches upstream
                                // more often.
                                probe.visit(instance, subschema, base, depth + 1, Entry::Descend);
                            },
                        );
                    });
                    if !failed {
                        all_failed = false;
                        held = true;
                        if keyword == "anyOf" {
                            break;
                        }
                    }
                }
                if all_failed {
                    self.failed = true;
                }
            }
            "not" => {
                // `evolve(schema=...).is_valid(instance)`: `is_valid`, then `iter_errors`.
                self.scoped(true, |probe| {
                    probe.with_paths(Some(keyword), None, None, |probe| {
                        probe.visit(instance, value, base, depth + 2, Entry::IsValid);
                    });
                });
            }
            "if" => {
                self.scoped(true, |probe| {
                    probe.visit(instance, value, base, depth + 2, Entry::IsValid);
                });
                for branch in ["then", "else"] {
                    if let Some(subschema) = schema.get(branch) {
                        self.with_paths(Some(branch), None, None, |probe| {
                            probe.visit(instance, subschema, base, depth + 1, Entry::Descend);
                        });
                    }
                }
            }
            "dependencies" | "dependentSchemas" => {
                let (Some(object), Value::Object(dependencies)) = (object, value) else {
                    return;
                };
                for (name, dependency) in dependencies {
                    if object.contains_key(name) && dependency.is_object() {
                        self.with_paths(
                            Some(keyword),
                            Some(PathItem::Key(name.clone())),
                            None,
                            |probe| {
                                probe.visit(instance, dependency, base, depth + 1, Entry::Descend);
                            },
                        );
                    }
                }
            }
            "propertyNames" => {
                let Some(object) = object else { return };
                for name in object.keys() {
                    let name = Value::String(name.clone());
                    self.with_paths(Some(keyword), None, None, |probe| {
                        probe.visit(&name, value, base, depth + 1, Entry::Descend);
                    });
                }
            }
            "contains" => {
                let Value::Array(items) = instance else {
                    return;
                };
                for (index, item) in items.iter().enumerate() {
                    self.scoped(true, |probe| {
                        probe.with_paths(
                            Some(keyword),
                            None,
                            Some(PathItem::Index(index)),
                            |probe| {
                                probe.visit(item, value, base, depth + 2, Entry::IsValid);
                            },
                        );
                    });
                }
            }
            "unevaluatedProperties" | "unevaluatedItems" => {
                // `find_evaluated_*_by_schema` walks the schema again, checking
                // subschemas with `descend` one level further down.
                self.evaluated(instance, schema, base, depth + 1, 0);
                if keyword == "unevaluatedProperties" {
                    if let Some(object) = object {
                        for (name, member) in object {
                            self.with_paths(
                                Some(keyword),
                                Some(PathItem::Key(name.clone())),
                                Some(PathItem::Key(name.clone())),
                                |probe| {
                                    probe.visit(member, value, base, depth + 1, Entry::Descend);
                                },
                            );
                        }
                    }
                } else if let Value::Array(items) = instance {
                    for (index, item) in items.iter().enumerate() {
                        self.with_paths(
                            Some(keyword),
                            None,
                            Some(PathItem::Index(index)),
                            |probe| {
                                probe.visit(item, value, base, depth + 2, Entry::IsValid);
                            },
                        );
                    }
                }
            }
            _ => {}
        }
    }

    /// `_validate_reference` returning `descend(..., resolver=...)`: the descend frame is
    /// at `depth + 1`, iterated by the reference keyword's frame at `depth`.
    fn descend(
        &mut self,
        instance: &Value,
        target: &'a Value,
        base: &'a str,
        depth: usize,
        keyword: Option<&'a str>,
        segment: Option<PathItem>,
    ) {
        self.with_paths(keyword, segment, None, |probe| {
            probe.visit(instance, target, base, depth + 1, Entry::Descend);
        });
    }

    /// `find_evaluated_property_keys_by_schema` (and the items variant), its frame at
    /// `depth`: references recurse one frame down, applicators check validity with a
    /// `descend` (through `_utils.is_valid`) and recurse.
    fn evaluated(
        &mut self,
        instance: &Value,
        schema: &'a Map<String, Value>,
        base: &'a str,
        depth: usize,
        hops: usize,
    ) {
        if hops > 64 || !self.reach(depth + 3) {
            return;
        }
        for keyword in ["$ref", "$dynamicRef"] {
            if let Some(Value::String(reference)) = schema.get(keyword) {
                let target = if keyword == "$dynamicRef" {
                    self.search.resolve_dynamic(base, reference)
                } else {
                    self.search.resolve(base, reference)
                };
                if let Some((Value::Object(target), target_base)) = target {
                    self.evaluated(instance, target, target_base, depth + 1, hops + 1);
                }
            }
        }
        if let Some(object) = pyobj::as_dict(instance) {
            for keyword in ["additionalProperties", "unevaluatedProperties"] {
                if let Some(subschema) = schema.get(keyword) {
                    for member in object.values() {
                        // generator, `is_valid`, `descend`
                        self.visit(member, subschema, base, depth + 3, Entry::Descend);
                    }
                }
            }
        }
        for keyword in ["allOf", "oneOf", "anyOf"] {
            if let Some(Value::Array(subschemas)) = schema.get(keyword) {
                for subschema in subschemas {
                    self.visit(instance, subschema, base, depth + 2, Entry::Descend);
                    if let Value::Object(subschema) = subschema {
                        self.evaluated(instance, subschema, base, depth + 1, hops + 1);
                    }
                }
            }
        }
        if let Some(Value::Object(dependent)) = schema.get("dependentSchemas") {
            for subschema in dependent.values() {
                if let Value::Object(subschema) = subschema {
                    self.evaluated(instance, subschema, base, depth + 1, hops + 1);
                }
            }
        }
        if let Some(condition) = schema.get("if") {
            self.visit(instance, condition, base, depth + 2, Entry::IsValid);
            for branch in ["if", "then", "else"] {
                if let Some(Value::Object(subschema)) = schema.get(branch) {
                    self.evaluated(instance, subschema, base, depth + 1, hops + 1);
                }
            }
        }
    }

    fn with_paths(
        &mut self,
        keyword: Option<&str>,
        segment: Option<PathItem>,
        member: Option<PathItem>,
        f: impl FnOnce(&mut Self),
    ) {
        let schema_len = self.schema_path.len();
        let instance_len = self.instance_path.len();
        if let Some(keyword) = keyword {
            self.schema_path.push(PathItem::Key(keyword.to_owned()));
        }
        if let Some(segment) = segment {
            self.schema_path.push(segment);
        }
        if let Some(member) = member {
            self.instance_path.push(member);
        }
        f(self);
        self.schema_path.truncate(schema_len);
        self.instance_path.truncate(instance_len);
    }

    /// `re.search(pattern, text)`; a pattern Python cannot compile matches nothing here
    /// (upstream would raise, which the validation itself reports).
    fn search_pattern(&mut self, pattern: &str, text: &str) -> bool {
        let regex = self
            .regexes
            .entry(pattern.to_owned())
            .or_insert_with(|| PyRegex::compile(pattern).ok());
        regex
            .as_ref()
            .is_some_and(|regex| regex.search(text).unwrap_or(false))
    }
}

/// The most frames any step from one instance level to the next can add in the
/// meta-schemas `schemas` (from a subschema's iterating frame to the `descend` frame of
/// a subschema applied to a member), or `None` if references can chain without bound.
pub(crate) fn frames_per_level(schemas: &Schemas<'_>) -> Option<usize> {
    let search = Search::new(&schemas.resources, schemas.ref_overrides, &[], &NULL, "");
    let draft4 = schemas.ref_overrides;
    let mut memo = std::collections::HashMap::new();
    let mut deepest = 0;
    // Every subschema of every resource can be a level's starting point.
    let mut stack: Vec<(&Value, &str)> = schemas
        .resources
        .iter()
        .map(|r| (r.contents, r.uri))
        .collect();
    let mut seen = std::collections::HashSet::new();
    while let Some((value, base)) = stack.pop() {
        if !seen.insert(std::ptr::from_ref(value)) {
            continue;
        }
        if let Value::Object(map) = value {
            deepest = deepest.max(level_cost(
                value,
                base,
                &search,
                draft4,
                &mut memo,
                &mut Vec::new(),
            )?);
            stack.extend(map.values().map(|child| (child, base)));
        } else if let Value::Array(items) = value {
            stack.extend(items.iter().map(|child| (child, base)));
        }
    }
    Some(deepest)
}

/// Frames from `schema`'s iterating frame to the furthest `descend` into a member of
/// the instance, through keywords that apply subschemas to the same instance.
fn level_cost<'a>(
    schema: &'a Value,
    base: &'a str,
    search: &Search<'a>,
    draft4: bool,
    memo: &mut std::collections::HashMap<*const Value, usize>,
    active: &mut Vec<*const Value>,
) -> Option<usize> {
    let Value::Object(map) = schema else {
        return Some(0);
    };
    let id = std::ptr::from_ref(schema);
    if let Some(&cost) = memo.get(&id) {
        return Some(cost);
    }
    if active.contains(&id) {
        return None;
    }
    active.push(id);
    let mut cost = 0usize;
    // A subschema applied to the same instance, its iterating frame `offset` above.
    macro_rules! same {
        ($sub:expr, $base:expr, $offset:expr) => {{
            let inner = level_cost($sub, $base, search, draft4, memo, active)?;
            cost = cost.max($offset + inner);
        }};
    }
    let keywords: Vec<(&String, &Value)> = match map.get("$ref") {
        Some(reference) if draft4 => map
            .iter()
            .filter(|(k, _)| k.as_str() == "$ref")
            .map(|(k, _)| (k, reference))
            .collect(),
        _ => map.iter().collect(),
    };
    for (keyword, value) in keywords {
        match keyword.as_str() {
            "properties"
            | "patternProperties"
            | "additionalProperties"
            | "items"
            | "prefixItems"
            | "additionalItems"
            | "propertyNames" => cost = cost.max(2),
            "contains" => cost = cost.max(3),
            "allOf" | "anyOf" | "oneOf" => {
                for sub in value.as_array().into_iter().flatten() {
                    same!(sub, base, 2);
                    // A later `oneOf` branch may be checked with `is_valid`.
                    same!(sub, base, 3);
                }
            }
            "not" | "if" => same!(value, base, 3),
            "then" | "else" => same!(value, base, 2),
            "dependencies" | "dependentSchemas" => {
                for sub in value.as_object().into_iter().flat_map(|m| m.values()) {
                    same!(sub, base, 2);
                }
            }
            "$ref" | "$dynamicRef" => {
                let Value::String(reference) = value else {
                    continue;
                };
                let target = if keyword == "$dynamicRef" {
                    search.resolve_dynamic(base, reference)
                } else {
                    search.resolve(base, reference)
                };
                if let Some((target, target_base)) = target {
                    same!(target, target_base, 2);
                }
            }
            // `find_evaluated_*_by_schema` re-walks the schema one frame per reference
            // or applicator, checking members (`is_valid` of a `descend`) on the way.
            "unevaluatedProperties" | "unevaluatedItems" => {
                let mut nested = active.clone();
                nested.retain(|p| *p != id);
                let walked = evaluated_cost(schema, base, search, draft4, memo, &mut nested, 0)?;
                cost = cost.max(2 + walked).max(3);
            }
            _ => {}
        }
    }
    active.pop();
    memo.insert(id, cost);
    Some(cost)
}

/// [`level_cost`] for the frames of `find_evaluated_*_by_schema` on `schema`.
fn evaluated_cost<'a>(
    schema: &'a Value,
    base: &'a str,
    search: &Search<'a>,
    draft4: bool,
    memo: &mut std::collections::HashMap<*const Value, usize>,
    active: &mut Vec<*const Value>,
    hops: usize,
) -> Option<usize> {
    let Value::Object(map) = schema else {
        return Some(0);
    };
    if hops > 32 {
        return None;
    }
    // Members: generator, `is_valid`, `descend`.
    let mut cost = 3;
    for keyword in ["$ref", "$dynamicRef"] {
        if let Some(Value::String(reference)) = map.get(keyword) {
            let target = if keyword == "$dynamicRef" {
                search.resolve_dynamic(base, reference)
            } else {
                search.resolve(base, reference)
            };
            if let Some((target, target_base)) = target {
                cost = cost.max(
                    1 + evaluated_cost(
                        target,
                        target_base,
                        search,
                        draft4,
                        memo,
                        active,
                        hops + 1,
                    )?,
                );
            }
        }
    }
    for keyword in ["allOf", "oneOf", "anyOf"] {
        for sub in map
            .get(keyword)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            cost = cost.max(2 + level_cost(sub, base, search, draft4, memo, active)?);
            cost = cost.max(1 + evaluated_cost(sub, base, search, draft4, memo, active, hops + 1)?);
        }
    }
    for keyword in ["if", "then", "else"] {
        if let Some(sub) = map.get(keyword) {
            cost = cost.max(3 + level_cost(sub, base, search, draft4, memo, active)?);
            cost = cost.max(1 + evaluated_cost(sub, base, search, draft4, memo, active, hops + 1)?);
        }
    }
    for sub in map
        .get("dependentSchemas")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|m| m.values())
    {
        cost = cost.max(1 + evaluated_cost(sub, base, search, draft4, memo, active, hops + 1)?);
    }
    Some(cost)
}

/// The most frames a single call chain adds on top of the iterating frames: `descend`'s
/// setup (8), a dynamic reference's first lookup (9), a regular expression parsed for
/// the first time through the `regex` format (`3 + 6 + 2 + 1` plus two per group level).
pub(crate) fn transient_bound(instance: &Value) -> usize {
    fn groups(value: &Value, deepest: &mut usize) {
        match value {
            Value::String(s) if s.contains('(') => *deepest = (*deepest).max(group_levels(s)),
            Value::Object(map) => {
                for (key, member) in map {
                    if key.contains('(') {
                        *deepest = (*deepest).max(group_levels(key));
                    }
                    groups(member, deepest);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| groups(item, deepest)),
            _ => {}
        }
    }
    let mut deepest = 0;
    groups(instance, &mut deepest);
    (12 + 2 * deepest).max(9)
}

fn group_levels(pattern: &str) -> usize {
    let (mut level, mut deepest) = (0usize, 0usize);
    for c in pattern.chars() {
        match c {
            '(' => {
                level += 1;
                deepest = deepest.max(level);
            }
            ')' => level = level.saturating_sub(1),
            _ => {}
        }
    }
    deepest
}

/// How many containers deep `value` nests (a scalar is 0).
pub(crate) fn nesting(value: &Value) -> usize {
    match value {
        Value::Object(map) => 1 + map.values().map(nesting).max().unwrap_or(0),
        Value::Array(items) => 1 + items.iter().map(nesting).max().unwrap_or(0),
        _ => 0,
    }
}

/// Where checking `instance` against the meta-schemas `schemas` would exceed the limit,
/// or `None` when it stays within it. The model runs only when a cheap bound (nesting
/// times the most frames a level can take) comes close to the limit.
///
/// `nesting` is how deep `instance` nests at most, when the caller knows (a document's
/// nesting bounds every value in it).
pub(crate) fn find<'a>(
    instance: &Value,
    schemas: &'a Schemas<'a>,
    budget: Budget,
    formats: bool,
    nesting: Option<usize>,
) -> Option<Probe<'a>> {
    let available = budget.available();
    let per_level = cached_frames_per_level(schemas);
    if let Some(per_level) = per_level {
        let levels = nesting.unwrap_or_else(|| self::nesting(instance)) + 1;
        let bound = |transient: usize| levels.saturating_mul(per_level).saturating_add(transient);
        // Patterns rarely nest groups: a generous transient first, the exact one if needed.
        if bound(40) <= available || bound(transient_bound(instance)) <= available {
            return None;
        }
    }
    let mut probe = Probe::new(schemas, budget, formats);
    probe.run(instance);
    probe.crossing.is_some().then_some(probe)
}

fn cached_frames_per_level(schemas: &Schemas<'_>) -> Option<usize> {
    static CACHE: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<usize, Option<usize>>>,
    > = std::sync::LazyLock::new(Default::default);
    let key = schemas
        .resources
        .first()
        .map_or(0, |r| std::ptr::from_ref(r.contents) as usize);
    if let Some(&cached) = CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
    {
        return cached;
    }
    let computed = frames_per_level(schemas);
    CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key, computed);
    computed
}

/// Whether upstream yields `error` (a top-level error of the same check, with absolute
/// paths) before it reaches `crossing`: it comes earlier in Python's evaluation order and
/// is not reported by a keyword (`oneOf`, `anyOf`) that evaluates the crossing first.
pub(crate) fn precedes(
    schemas: &Schemas<'_>,
    instance: &Value,
    error: &crate::error::ValidationError,
    crossing: &Crossing,
) -> bool {
    let (Some(details), Some(root)) = (error.schema_error.as_deref(), schemas.resources.first())
    else {
        return false;
    };
    let order = |schema_path: &[PathItem], instance_path: &[PathItem]| {
        crate::schema_path::python_order(
            &schemas.resources,
            root,
            schema_path,
            instance,
            instance_path,
        )
    };
    let first = order(&details.schema_path, &details.instance_path);
    let deep = order(&crossing.schema_path, &crossing.instance_path);
    first < deep && !deep.starts_with(&first)
}

/// A copy of `instance` in which every value at one of `locations` keeps its own shape
/// (type, keys, length) but none of what it contains.
pub(crate) fn pruned(instance: &Value, locations: &[Vec<PathItem>]) -> Value {
    fn prune(value: &mut Value, path: &[PathItem]) {
        match path.split_first() {
            None => match value {
                Value::Object(map) => map.values_mut().for_each(|member| *member = Value::Null),
                Value::Array(items) => items.iter_mut().for_each(|item| *item = Value::Null),
                _ => {}
            },
            Some((PathItem::Key(key), rest)) => {
                if let Some(member) = value.as_object_mut().and_then(|map| map.get_mut(key)) {
                    prune(member, rest);
                }
            }
            Some((PathItem::Index(index), rest)) => {
                if let Some(item) = value.as_array_mut().and_then(|items| items.get_mut(*index)) {
                    prune(item, rest);
                }
            }
        }
    }
    let mut copy = instance.clone();
    for location in locations {
        prune(&mut copy, location);
    }
    copy
}
