//! Reconstructs jsonschema's `relative_schema_path` for an error.
//!
//! Python's jsonschema builds an error's schema path from the keywords it descends
//! through, leaving out `$ref` hops. The `jsonschema` crate reports where the failing
//! keyword lives (an absolute location in some resource) and an evaluation path that
//! may skip references it inlined, so the Python path is recovered by searching the
//! schema from the validation root along the error's instance path for the subschema
//! that contains the failing keyword.

use percent_encoding::percent_decode_str;
use serde_json::Value;

use crate::pyregex::PyRegex;
use crate::pyrepr::PathItem;

/// Applicator steps that do not consume the instance, followed at most this many times
/// in a row (meta-schemas are recursive through `$ref`).
const MAX_STEPS_PER_INSTANCE_LEVEL: usize = 24;

/// A schema resource with the URI its references resolve against.
#[derive(Clone, Copy)]
pub(crate) struct Resource<'a> {
    pub uri: &'a str,
    pub contents: &'a Value,
}

pub(crate) struct Search<'a> {
    resources: &'a [Resource<'a>],
    /// Whether `$ref` siblings are ignored (Draft 4, 6 and 7).
    ref_overrides: bool,
    instance_path: &'a [PathItem],
    target: *const Value,
    keyword: &'a str,
}

impl<'a> Search<'a> {
    pub(crate) fn new(
        resources: &'a [Resource<'a>],
        ref_overrides: bool,
        instance_path: &'a [PathItem],
        target: &'a Value,
        keyword: &'a str,
    ) -> Self {
        Self {
            resources,
            ref_overrides,
            instance_path,
            target: std::ptr::from_ref(target),
            keyword,
        }
    }

    /// The schema path from `root` (in resource `base`) to the failing keyword.
    pub(crate) fn run(&self, root: &'a Value, base: &'a str) -> Option<Vec<PathItem>> {
        let mut path = Vec::new();
        if self.visit(root, base, 0, 0, &mut path) {
            path.push(PathItem::Key(self.keyword.to_owned()));
            Some(path)
        } else {
            None
        }
    }

    fn visit(
        &self,
        schema: &'a Value,
        base: &'a str,
        consumed: usize,
        steps: usize,
        path: &mut Vec<PathItem>,
    ) -> bool {
        if consumed == self.instance_path.len()
            && std::ptr::eq(schema, self.target)
            && schema.get(self.keyword).is_some()
        {
            return true;
        }
        let Value::Object(map) = schema else {
            return false;
        };
        if steps > MAX_STEPS_PER_INSTANCE_LEVEL {
            return false;
        }

        // `$ref` adds nothing to the path; in older drafts it hides its siblings.
        if let Some(Value::String(reference)) = map.get("$ref") {
            if let Some((target, target_base)) = self.resolve(base, reference) {
                if self.visit(target, target_base, consumed, steps + 1, path) {
                    return true;
                }
            }
            if self.ref_overrides {
                return false;
            }
        }
        if let Some(Value::String(reference)) = map.get("$dynamicRef") {
            if let Some((target, target_base)) = self.resolve_dynamic(base, reference) {
                if self.descend(
                    target,
                    target_base,
                    consumed,
                    steps + 1,
                    path,
                    &["$dynamicRef"],
                ) {
                    return true;
                }
            }
        }

        // Keywords that apply subschemas to the same instance.
        for keyword in ["allOf", "anyOf", "oneOf"] {
            if let Some(Value::Array(items)) = map.get(keyword) {
                for (index, sub) in items.iter().enumerate() {
                    path.push(PathItem::Key(keyword.to_owned()));
                    path.push(PathItem::Index(index));
                    if self.visit(sub, base, consumed, steps + 1, path) {
                        return true;
                    }
                    path.truncate(path.len() - 2);
                }
            }
        }
        // `if` errors are never reported; `then`/`else` are recorded without `if`.
        for keyword in ["not", "then", "else"] {
            if let Some(sub) = map.get(keyword) {
                if self.descend(sub, base, consumed, steps + 1, path, &[keyword]) {
                    return true;
                }
            }
        }
        for keyword in ["dependencies", "dependentSchemas"] {
            if let Some(Value::Object(deps)) = map.get(keyword) {
                for (name, sub) in deps {
                    if self.descend(sub, base, consumed, steps + 1, path, &[keyword, name]) {
                        return true;
                    }
                }
            }
        }

        // Keywords that descend into part of the instance.
        let Some(step) = self.instance_path.get(consumed) else {
            return false;
        };
        let next = consumed + 1;
        match step {
            PathItem::Key(key) => {
                let properties = map.get("properties").and_then(Value::as_object);
                if let Some(sub) = properties.and_then(|p| p.get(key)) {
                    if self.descend(sub, base, next, 0, path, &["properties", key]) {
                        return true;
                    }
                }
                let mut matched = false;
                if let Some(Value::Object(patterns)) = map.get("patternProperties") {
                    for (pattern, sub) in patterns {
                        let matches = PyRegex::compile(pattern)
                            .ok()
                            .and_then(|regex| regex.search(key).ok())
                            .unwrap_or(false);
                        if matches {
                            matched = true;
                            if self.descend(
                                sub,
                                base,
                                next,
                                0,
                                path,
                                &["patternProperties", pattern],
                            ) {
                                return true;
                            }
                        }
                    }
                }
                let declared = properties.is_some_and(|p| p.contains_key(key));
                if !declared && !matched {
                    for keyword in ["additionalProperties", "unevaluatedProperties"] {
                        if let Some(sub) = map.get(keyword) {
                            if self.descend(sub, base, next, 0, path, &[keyword]) {
                                return true;
                            }
                        }
                    }
                }
            }
            PathItem::Index(index) => {
                let prefix = match map.get("prefixItems") {
                    Some(Value::Array(prefix)) => {
                        if let Some(sub) = prefix.get(*index) {
                            path.push(PathItem::Key("prefixItems".to_owned()));
                            path.push(PathItem::Index(*index));
                            if self.visit(sub, base, next, 0, path) {
                                return true;
                            }
                            path.truncate(path.len() - 2);
                        }
                        prefix.len()
                    }
                    _ => 0,
                };
                match map.get("items") {
                    // Tuple form (Draft 4 to 2019-09).
                    Some(Value::Array(items)) => {
                        if let Some(sub) = items.get(*index) {
                            path.push(PathItem::Key("items".to_owned()));
                            path.push(PathItem::Index(*index));
                            if self.visit(sub, base, next, 0, path) {
                                return true;
                            }
                            path.truncate(path.len() - 2);
                        } else if let Some(sub) = map.get("additionalItems") {
                            if self.descend(sub, base, next, 0, path, &["additionalItems"]) {
                                return true;
                            }
                        }
                    }
                    Some(sub)
                        if *index >= prefix
                            && self.descend(sub, base, next, 0, path, &["items"]) =>
                    {
                        return true;
                    }
                    _ => {}
                }
                for keyword in ["contains", "unevaluatedItems"] {
                    if let Some(sub) = map.get(keyword) {
                        if self.descend(sub, base, next, 0, path, &[keyword]) {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    fn descend(
        &self,
        schema: &'a Value,
        base: &'a str,
        consumed: usize,
        steps: usize,
        path: &mut Vec<PathItem>,
        segments: &[&str],
    ) -> bool {
        let len = path.len();
        path.extend(segments.iter().map(|s| PathItem::Key((*s).to_owned())));
        if self.visit(schema, base, consumed, steps, path) {
            return true;
        }
        path.truncate(len);
        false
    }

    fn resource(&self, uri: &str) -> Option<&Resource<'a>> {
        let uri = uri.trim_end_matches('#');
        self.resources
            .iter()
            .find(|r| r.uri.trim_end_matches('#') == uri)
    }

    /// Resolves a `$ref` relative to the resource `base`.
    fn resolve(&self, base: &'a str, reference: &str) -> Option<(&'a Value, &'a str)> {
        let (uri, fragment) = reference.split_once('#').unwrap_or((reference, ""));
        let resource = if uri.is_empty() {
            self.resource(base)?
        } else {
            let absolute = url::Url::parse(base.trim_end_matches('#'))
                .ok()
                .and_then(|base| base.join(uri).ok())
                .map_or_else(|| uri.to_owned(), String::from);
            self.resource(&absolute).or_else(|| self.resource(uri))?
        };
        let target = if fragment.is_empty() {
            resource.contents
        } else {
            let decoded = percent_decode_str(fragment).decode_utf8_lossy();
            resource.contents.pointer(&decoded)?
        };
        Some((target, resource.uri))
    }

    /// Resolves a `$dynamicRef` like `#meta` to the outermost resource declaring that
    /// `$dynamicAnchor` (the validation root comes first in `resources`).
    fn resolve_dynamic(&self, base: &'a str, reference: &str) -> Option<(&'a Value, &'a str)> {
        let anchor = reference.strip_prefix('#')?;
        for resource in self.resources {
            if let Some(found) = find_anchor(resource.contents, anchor) {
                return Some((found, resource.uri));
            }
        }
        self.resolve(base, reference)
    }
}

fn find_anchor<'v>(value: &'v Value, anchor: &str) -> Option<&'v Value> {
    match value {
        Value::Object(map) => {
            if map.get("$dynamicAnchor").and_then(Value::as_str) == Some(anchor) {
                return Some(value);
            }
            map.values().find_map(|child| find_anchor(child, anchor))
        }
        Value::Array(items) => items.iter().find_map(|child| find_anchor(child, anchor)),
        _ => None,
    }
}

/// A sort key placing errors in the order Python's jsonschema yields them: depth-first
/// through each schema's keywords in document order, and through instance members in
/// the order the keyword visits them.
///
/// `schema_path` is the error's (absolute) Python schema path from `root`, and
/// `instance_path` its instance path from `instance`.
pub(crate) fn python_order(
    resources: &[Resource<'_>],
    root: &Resource<'_>,
    schema_path: &[PathItem],
    instance: &Value,
    instance_path: &[PathItem],
) -> Vec<usize> {
    let search = Search {
        resources,
        ref_overrides: false,
        instance_path,
        target: std::ptr::null(),
        keyword: "",
    };
    let mut key = Vec::new();
    let mut current = root.contents;
    let mut base = root.uri;
    let mut instance = instance;
    let mut consumed = 0;
    let mut i = 0;
    // Bounds `$ref` chains that never reach the next path segment.
    let mut hops = 0;
    while let Some(PathItem::Key(segment)) = schema_path.get(i) {
        let Value::Object(map) = current else { break };
        if !map.contains_key(segment) {
            let Some(Value::String(reference)) = map.get("$ref") else {
                break;
            };
            let Some((target, target_base)) = search.resolve(base, reference) else {
                break;
            };
            hops += 1;
            if hops > MAX_STEPS_PER_INSTANCE_LEVEL {
                break;
            }
            key.push(position(map, "$ref"));
            current = target;
            base = target_base;
            continue;
        }
        hops = 0;
        key.push(position(map, segment));
        let value = &map[segment.as_str()];
        // The instance member the next step descends into.
        let member = instance_path.get(consumed);
        let mut consume = |key: &mut Vec<usize>, instance: &mut &Value| {
            if let Some(member) = member {
                key.push(member_position(instance, member));
                *instance = member_value(instance, member).unwrap_or(&Value::Null);
                consumed += 1;
            }
        };
        match segment.as_str() {
            "properties" | "patternProperties" | "dependencies" | "dependentSchemas" => {
                let Some(PathItem::Key(name)) = schema_path.get(i + 1) else {
                    break;
                };
                let Value::Object(members) = value else { break };
                key.push(position(members, name));
                if segment == "patternProperties" {
                    consume(&mut key, &mut instance);
                } else if segment == "properties" {
                    // `properties` walks the schema's members, not the instance's.
                    if let Some(member) = member {
                        instance = member_value(instance, member).unwrap_or(&Value::Null);
                        consumed += 1;
                    }
                }
                current = &members[name.as_str()];
                i += 2;
            }
            "allOf" | "anyOf" | "oneOf" | "prefixItems" => {
                let Some(PathItem::Index(index)) = schema_path.get(i + 1) else {
                    break;
                };
                key.push(*index);
                if segment == "prefixItems" {
                    consume(&mut key, &mut instance);
                }
                let Some(sub) = value.get(*index) else { break };
                current = sub;
                i += 2;
            }
            "items" if value.is_array() => {
                let Some(PathItem::Index(index)) = schema_path.get(i + 1) else {
                    break;
                };
                key.push(*index);
                consume(&mut key, &mut instance);
                let Some(sub) = value.get(*index) else { break };
                current = sub;
                i += 2;
            }
            "items"
            | "additionalItems"
            | "unevaluatedItems"
            | "contains"
            | "additionalProperties"
            | "unevaluatedProperties" => {
                consume(&mut key, &mut instance);
                current = value;
                i += 1;
            }
            "$dynamicRef" => {
                let Value::String(reference) = value else {
                    break;
                };
                let Some((target, target_base)) = search.resolve_dynamic(base, reference) else {
                    break;
                };
                current = target;
                base = target_base;
                i += 1;
            }
            // `not`, `then`, `else`, or the failing keyword itself.
            _ => {
                current = value;
                i += 1;
            }
        }
    }
    key
}

fn position(map: &serde_json::Map<String, Value>, key: &str) -> usize {
    map.keys().position(|k| k == key).unwrap_or(usize::MAX)
}

fn member_position(instance: &Value, member: &PathItem) -> usize {
    match (instance, member) {
        (Value::Object(map), PathItem::Key(key)) => position(map, key),
        (_, PathItem::Index(index)) => *index,
        _ => usize::MAX,
    }
}

fn member_value<'v>(instance: &'v Value, member: &PathItem) -> Option<&'v Value> {
    match (instance, member) {
        (Value::Object(map), PathItem::Key(key)) => map.get(key),
        (Value::Array(items), PathItem::Index(index)) => items.get(*index),
        _ => None,
    }
}
