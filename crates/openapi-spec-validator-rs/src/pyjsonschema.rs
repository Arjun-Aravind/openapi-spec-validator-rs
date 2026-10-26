//! Python jsonschema's validators, for `default` values in OpenAPI 3.1 and 3.2 documents.
//!
//! Upstream validates a `default` with openapi-schema-validator's `OAS31Validator` or
//! `OAS32Validator`: jsonschema's Draft 2020-12 validator with OpenAPI formats. This is a
//! port of that machinery rather than a JSON Schema implementation: keywords are applied
//! in schema order, lazily, with Python's comparison and iteration semantics, so a
//! malformed keyword value fails where and how it fails upstream, and messages, paths
//! and error order are jsonschema's.
//!
//! Two jsonschema behaviours matter beyond the keywords themselves:
//! * a subschema whose `$schema` names another draft is validated with that draft's
//!   keywords and types (`Validator.evolve` calls `validator_for`);
//! * entering a subschema with an `$id` moves the base URI of the `$ref`s below it
//!   (`Resolver.in_subresource`), except under `OAS32Validator`, whose `referencing`
//!   specification ignores `$id`.

use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;

use serde_json::{Map, Value};

use crate::document::{Crash, Documents, LookupError, Node};
use crate::formats::{self, FormatOutcome, FormatSet};
use crate::meta::Dialect;
use crate::pyobj::{self, Num, json_equal, num_cmp, py_str, truthy};
use crate::pyregex::{self, PyRegex};
use crate::pyrepr::{PathItem, foreign, pformat, repr, repr_str};
use crate::values::{ValueError, multiple_of, number, unsupported_comparison};
use crate::version::python_type_name;

/// Python frames upstream can still use when it starts validating a `default`: its
/// recursion limit (1000) minus the frames of the command line and the walk above it.
/// Measured with `$ref` chains and recursive schemas under a component's `default`.
const MAX_FRAMES: usize = 982;

/// A jsonschema validator class, i.e. a dialect with its keywords and type checker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Draft4,
    Draft6,
    Draft7,
    Draft201909,
    Draft202012,
    Oas31,
    Oas32,
}

impl From<Dialect> for Class {
    fn from(dialect: Dialect) -> Self {
        match dialect {
            Dialect::Draft4 => Self::Draft4,
            Dialect::Draft6 => Self::Draft6,
            Dialect::Draft7 => Self::Draft7,
            Dialect::Draft201909 => Self::Draft201909,
            Dialect::Draft202012 => Self::Draft202012,
            Dialect::Oas31 => Self::Oas31,
            Dialect::Oas32 => Self::Oas32,
        }
    }
}

impl Class {
    /// `validator_for(schema, default=self)`, as `evolve` calls it. Draft 3 has no
    /// counterpart here, so its subschemas keep the surrounding class.
    fn evolve(self, schema: &Value) -> Result<Self, Crash> {
        // `"$schema" not in schema`, then `schema["$schema"]`.
        let map = match schema {
            Value::Bool(_) => return Ok(self),
            Value::Object(map) => map,
            Value::String(s) if s.contains("$schema") => {
                return Err(Crash(crate::pyobj::string_indices()));
            }
            Value::Array(items) if items.iter().any(|i| i.as_str() == Some("$schema")) => {
                return Err(Crash(
                    "list indices must be integers or slices, not str".to_owned(),
                ));
            }
            Value::String(_) | Value::Array(_) => return Ok(self),
            other => return Err(not_iterable_argument(other)),
        };
        // `_META_SCHEMAS` is a `URIDict`, which normalizes keys with `urlsplit`.
        match map.get("$schema") {
            None => Ok(self),
            Some(Value::String(uri)) => Ok(Dialect::from_uri(uri).map_or(self, Self::from)),
            Some(other) if !truthy(other) => Ok(self),
            Some(other) => Err(no_attribute(other, "decode")),
        }
    }

    /// Whether the class's `VALIDATORS` has `keyword`.
    fn knows(self, keyword: &str) -> bool {
        use Class::{Draft4, Draft6, Draft7, Draft201909, Draft202012, Oas31, Oas32};
        match keyword {
            "$ref"
            | "additionalProperties"
            | "allOf"
            | "anyOf"
            | "enum"
            | "format"
            | "items"
            | "maxItems"
            | "maxLength"
            | "maxProperties"
            | "maximum"
            | "minItems"
            | "minLength"
            | "minProperties"
            | "minimum"
            | "multipleOf"
            | "not"
            | "oneOf"
            | "pattern"
            | "patternProperties"
            | "properties"
            | "required"
            | "type"
            | "uniqueItems" => true,
            "additionalItems" => matches!(self, Draft4 | Draft6 | Draft7 | Draft201909),
            "dependencies" => matches!(self, Draft4 | Draft6 | Draft7),
            "const" | "contains" | "exclusiveMaximum" | "exclusiveMinimum" | "propertyNames" => {
                self != Draft4
            }
            "if" => !matches!(self, Draft4 | Draft6),
            "$recursiveRef" => self == Draft201909,
            "dependentRequired"
            | "dependentSchemas"
            | "unevaluatedItems"
            | "unevaluatedProperties" => {
                matches!(self, Draft201909 | Draft202012 | Oas31 | Oas32)
            }
            "$dynamicRef" | "prefixItems" => matches!(self, Draft202012 | Oas31 | Oas32),
            _ => false,
        }
    }

    /// Draft 4 to 7 apply only `$ref` when it is present (`ignore_ref_siblings`).
    fn ignores_ref_siblings(self) -> bool {
        matches!(self, Self::Draft4 | Self::Draft6 | Self::Draft7)
    }

    /// The `$id` of `schema` as the class's `referencing` specification reads it.
    fn id_of(self, schema: &Value) -> Result<Option<&Value>, Crash> {
        match self {
            // `OAS32Validator` extends `OAS31Validator` after the latter's meta-schema was
            // replaced by the OAS dialect, which `referencing` does not know: opaque.
            Self::Oas32 => Ok(None),
            Self::Draft201909 | Self::Draft202012 | Self::Oas31 => match schema {
                Value::Object(map) => Ok(map.get("$id").filter(|id| !id.is_null())),
                Value::Bool(_) => Ok(None),
                other => Err(no_attribute(other, "get")),
            },
            Self::Draft4 | Self::Draft6 | Self::Draft7 => {
                let has_ref = match schema {
                    Value::Bool(_) => return Ok(None),
                    Value::Object(map) => map.contains_key("$ref"),
                    Value::String(s) => s.contains("$ref"),
                    Value::Array(items) => items.iter().any(|i| i.as_str() == Some("$ref")),
                    other => return Err(not_iterable_argument(other)),
                };
                if has_ref {
                    return Ok(None);
                }
                let Value::Object(map) = schema else {
                    return Err(no_attribute(schema, "get"));
                };
                let key = if self == Self::Draft4 { "id" } else { "$id" };
                match map.get(key) {
                    None | Some(Value::Null) => Ok(None),
                    Some(Value::String(id)) if id.starts_with('#') => Ok(None),
                    Some(id @ Value::String(_)) => Ok(Some(id)),
                    Some(other) => Err(no_attribute(other, "startswith")),
                }
            }
        }
    }

    /// `TypeChecker.is_type`; `None` for a type the checker does not define.
    fn is_type(self, instance: &Value, name: &str) -> Option<bool> {
        Some(match name {
            "array" => instance.is_array(),
            "boolean" => instance.is_boolean(),
            "integer" => match instance {
                Value::Number(n) => {
                    n.is_i64()
                        || n.is_u64()
                        // Draft 6 and later count floats with an integral value.
                        || (self != Self::Draft4 && n.as_f64().is_some_and(|f| f.fract() == 0.0))
                }
                _ => false,
            },
            "null" => instance.is_null(),
            "number" => instance.is_number(),
            "object" => pyobj::as_dict(instance).is_some(),
            "string" => instance.is_string(),
            _ => return None,
        })
    }
}

/// A validator instance: its class and its resolver's base URI.
#[derive(Clone, Debug)]
struct Validator {
    class: Class,
    base: Rc<str>,
}

type Errors = Vec<ValueError>;

/// Runs one `default` validation.
pub(crate) struct Evaluator<'d, 'a, 'r> {
    docs: &'d Documents<'a>,
    formats: FormatSet,
    regexes: &'r mut HashMap<String, Result<PyRegex, String>>,
    /// Python frames in use, to reproduce `RecursionError`.
    frames: usize,
    /// How many errors the innermost consumer takes before it stops iterating:
    /// jsonschema's validation is a chain of generators, so nothing after the error a
    /// consumer stops at is ever evaluated (exceptions included).
    limit: usize,
}

impl<'d, 'a, 'r> Evaluator<'d, 'a, 'r> {
    /// `OAS3xValidator(schema).iter_errors(instance)`, taking at most `limit` errors.
    pub(crate) fn run(
        docs: &'d Documents<'a>,
        formats: FormatSet,
        regexes: &'r mut HashMap<String, Result<PyRegex, String>>,
        schema: Node<'d>,
        instance: &Value,
        limit: usize,
    ) -> Result<Errors, Crash> {
        let class = if formats == FormatSet::Oas32 {
            Class::Oas32
        } else {
            Class::Oas31
        };
        let validator = Validator {
            class,
            base: docs.uri(schema.doc).into(),
        };
        let mut evaluator = Self {
            docs,
            formats,
            regexes,
            frames: 0,
            limit,
        };
        let mut errors = Vec::new();
        evaluator.iter_errors(&validator, schema.value, instance, &mut errors)?;
        Ok(errors)
    }
}

impl<'d> Evaluator<'d, '_, '_> {
    fn enter(&mut self, frames: usize) -> Result<(), Crash> {
        crate::context::check();
        self.frames += frames;
        if self.frames > MAX_FRAMES {
            return Err(Crash::recursion());
        }
        Ok(())
    }

    fn leave(&mut self, frames: usize) {
        self.frames -= frames;
    }

    /// Whether the current consumer has the errors it wants.
    fn full(&self, errors: &Errors) -> bool {
        errors.len() >= self.limit
    }

    /// The errors of `run`, collected for a consumer that takes at most `limit` of them
    /// (`next(...)`, `list(...)`).
    fn collect(
        &mut self,
        limit: usize,
        run: impl FnOnce(&mut Self, &mut Errors) -> Result<(), Crash>,
    ) -> Result<Errors, Crash> {
        let outer = std::mem::replace(&mut self.limit, limit);
        let mut errors = Vec::new();
        let result = run(self, &mut errors);
        self.limit = outer;
        result.map(|()| errors)
    }

    /// `validator.evolve(schema=schema)`: the class `validator_for` picks, checked like
    /// `__attrs_post_init__` checks the schema's keywords.
    fn evolve(&self, validator: &Validator, schema: &Value) -> Result<Validator, Crash> {
        let class = validator.class.evolve(schema)?;
        if !schema.is_boolean() {
            keywords(class, schema)?;
        }
        Ok(Validator {
            class,
            base: validator.base.clone(),
        })
    }

    /// `validator.evolve(schema=schema).is_valid(instance)`.
    fn is_valid(
        &mut self,
        validator: &Validator,
        schema: &Value,
        instance: &Value,
    ) -> Result<bool, Crash> {
        let evolved = self.evolve(validator, schema)?;
        self.is_valid_with(&evolved, schema, instance)
    }

    /// `is_valid(instance)` of an already evolved validator.
    fn is_valid_with(
        &mut self,
        validator: &Validator,
        schema: &Value,
        instance: &Value,
    ) -> Result<bool, Crash> {
        self.enter(1)?;
        let errors = self.collect(1, |this, errors| {
            this.iter_errors(validator, schema, instance, errors)
        })?;
        self.leave(1);
        Ok(errors.is_empty())
    }

    /// Whether `descend(instance, schema)` yields nothing (`next(...) is None`).
    fn descends_cleanly(
        &mut self,
        validator: &Validator,
        instance: &Value,
        schema: &Value,
    ) -> Result<bool, Crash> {
        let errors = self.collect(1, |this, errors| {
            this.descend(validator, instance, schema, None, None, None, errors)
        })?;
        Ok(errors.is_empty())
    }

    /// `Validator.iter_errors`: the keywords of the validator's own schema.
    fn iter_errors(
        &mut self,
        validator: &Validator,
        schema: &Value,
        instance: &Value,
        errors: &mut Errors,
    ) -> Result<(), Crash> {
        self.enter(1)?;
        match schema {
            Value::Bool(true) => {}
            Value::Bool(false) => errors.push(false_schema_error(instance)),
            _ => {
                let (map, ref_only) = keywords(validator.class, schema)?;
                self.apply(validator, map, ref_only, schema, instance, errors)?;
            }
        }
        self.leave(1);
        Ok(())
    }

    /// `Validator.descend`: validates `instance` against a subschema and prefixes the
    /// resulting errors' paths.
    #[allow(clippy::too_many_arguments)]
    fn descend(
        &mut self,
        validator: &Validator,
        instance: &Value,
        schema: &Value,
        path: Option<PathItem>,
        schema_path: Option<PathItem>,
        resolver: Option<Rc<str>>,
        errors: &mut Errors,
    ) -> Result<(), Crash> {
        self.enter(1)?;
        match schema {
            Value::Bool(true) => {}
            // Returned before any path is recorded.
            Value::Bool(false) => errors.push(false_schema_error(instance)),
            _ => {
                let base = match resolver {
                    Some(base) => base,
                    None => self.in_subresource(validator, schema)?,
                };
                let class = validator.class.evolve(schema)?;
                // Which keywords apply is the descending validator's choice; which
                // functions implement them is the evolved one's.
                let (map, ref_only) = keywords(validator.class, schema)?;
                let evolved = Validator { class, base };
                let start = errors.len();
                self.apply(&evolved, map, ref_only, schema, instance, errors)?;
                for error in &mut errors[start..] {
                    if let Some(item) = &path {
                        error.instance_path.push_front(item.clone());
                    }
                    if let Some(item) = &schema_path {
                        error.schema_path.push_front(item.clone());
                    }
                }
            }
        }
        self.leave(1);
        Ok(())
    }

    /// `Resolver.in_subresource`: the base URI below `schema`.
    fn in_subresource(&self, validator: &Validator, schema: &Value) -> Result<Rc<str>, Crash> {
        match validator.class.id_of(schema)? {
            None => Ok(validator.base.clone()),
            Some(Value::String(id)) => Ok(Documents::join(&validator.base, id).into()),
            // `urljoin(base, id)` returns `base` for an empty `id`.
            Some(id) if !truthy(id) => Ok(validator.base.clone()),
            Some(_) => Err(Crash("Cannot mix str and non-str arguments".to_owned())),
        }
    }

    /// Applies `schema`'s keywords (only `$ref` if `ref_only`) and records, for each
    /// error, what `descend`/`iter_errors` add: the keyword, the subschema and instance,
    /// and the keyword's step in the schema path.
    fn apply(
        &mut self,
        validator: &Validator,
        map: &Map<String, Value>,
        ref_only: bool,
        schema: &Value,
        instance: &Value,
        errors: &mut Errors,
    ) -> Result<(), Crash> {
        let entries: Box<dyn Iterator<Item = (&String, &Value)>> = if ref_only {
            Box::new(map.iter().filter(|(k, _)| *k == "$ref"))
        } else {
            Box::new(map.iter())
        };
        for (keyword, value) in entries {
            if !validator.class.knows(keyword) {
                continue;
            }
            if self.full(errors) {
                break;
            }
            let start = errors.len();
            self.enter(1)?;
            self.keyword(validator, keyword, value, map, instance, errors)?;
            self.leave(1);
            for error in &mut errors[start..] {
                if error.keyword.is_none() {
                    error.keyword = Some(keyword.clone());
                }
                if !error.located {
                    error.located = true;
                    error.schema = schema.clone();
                    error.instance = instance.clone();
                }
                if keyword != "if" && keyword != "$ref" {
                    error.schema_path.push_front(PathItem::Key(keyword.clone()));
                }
            }
        }
        Ok(())
    }

    fn compile(&mut self, pattern: &str) -> Result<&PyRegex, Crash> {
        self.regexes
            .entry(pattern.to_owned())
            .or_insert_with(|| PyRegex::compile(pattern).map_err(|e| e.python_str(pattern)))
            .as_ref()
            .map_err(|message| Crash(message.clone()))
    }

    /// `re.search(pattern, text)`.
    fn search(&mut self, pattern: &Value, text: &str) -> Result<bool, Crash> {
        let Value::String(pattern) = pattern else {
            return Err(Crash(
                "first argument must be string or compiled pattern".to_owned(),
            ));
        };
        self.compile(pattern)?.search(text).map_err(Crash)
    }

    /// `_validate_reference`: follows `$ref` / `$dynamicRef` (jsonschema 4.26 resolves
    /// both statically).
    fn reference(
        &mut self,
        validator: &Validator,
        reference: &Value,
        instance: &Value,
        errors: &mut Errors,
    ) -> Result<(), Crash> {
        let (target, base) = self
            .lookup(validator, reference)
            .map_err(LookupError::wrapped)?;
        self.descend(validator, instance, target, None, None, Some(base), errors)
    }

    /// `validator._resolver.lookup(reference)`: the target and its resolver's base URI.
    fn lookup(
        &self,
        validator: &Validator,
        reference: &Value,
    ) -> Result<(&'d Value, Rc<str>), LookupError> {
        let Value::String(reference) = reference else {
            return Err(no_attribute(reference, "startswith").into());
        };
        let docs: &'d Documents<'_> = self.docs;
        let target = docs.lookup_from(&validator.base, reference)?;
        Ok((target.value, docs.uri(target.doc).into()))
    }

    /// `validator.evolve(schema=target, _resolver=resolved.resolver)` for a reference
    /// that `find_evaluated_*` follows (unwrapped `referencing` errors).
    fn referenced(
        &self,
        validator: &Validator,
        reference: &Value,
    ) -> Result<(Validator, &'d Value), Crash> {
        let (target, base) = self.lookup(validator, reference)?;
        let evolved = self.evolve(validator, target)?;
        Ok((
            Validator {
                class: evolved.class,
                base,
            },
            target,
        ))
    }

    #[allow(clippy::too_many_lines)]
    fn keyword(
        &mut self,
        validator: &Validator,
        keyword: &str,
        value: &Value,
        schema: &Map<String, Value>,
        instance: &Value,
        errors: &mut Errors,
    ) -> Result<(), Crash> {
        let class = validator.class;
        let inst = || repr(instance);
        match keyword {
            "$ref" | "$dynamicRef" => self.reference(validator, value, instance, errors)?,
            "$recursiveRef" => {
                // `lookup_recursive_ref`; a spec's root never has `$recursiveAnchor`.
                let (target, base) = self.lookup(validator, &Value::from("#"))?;
                self.descend(validator, instance, target, None, None, Some(base), errors)?;
            }
            "additionalItems" => {
                let Value::Array(items) = instance else {
                    return Ok(());
                };
                let declared = schema.get("items");
                if declared.is_none_or(Value::is_object) {
                    return Ok(());
                }
                let declared = py_len(declared.unwrap_or(&Value::Null))?;
                if value.is_object() {
                    for (index, item) in items.iter().enumerate().skip(declared) {
                        let path = Some(PathItem::Index(index));
                        self.descend(validator, item, value, path, None, None, errors)?;
                        if self.full(errors) {
                            break;
                        }
                    }
                } else if !truthy(value) && items.len() > declared {
                    errors.push(error(format!(
                        "Additional items are not allowed ({} unexpected)",
                        extras_msg(&items[declared..])
                    )));
                }
            }
            "additionalProperties" => {
                let Some(object) = pyobj::as_dict(instance) else {
                    return Ok(());
                };
                // A set upstream: its iteration order varies between runs; this keeps
                // the instance's order.
                let extras = self.additional_properties(object, schema)?;
                if value.is_object() {
                    for extra in extras {
                        let path = Some(PathItem::Key(extra.to_owned()));
                        self.descend(validator, &object[extra], value, path, None, None, errors)?;
                        if self.full(errors) {
                            break;
                        }
                    }
                } else if !truthy(value) && !extras.is_empty() {
                    let mut sorted = extras;
                    sorted.sort_unstable();
                    let listed = sorted.iter().map(|e| repr_str(e)).collect::<Vec<_>>();
                    let message = if schema.contains_key("patternProperties") {
                        let mut patterns = py_iter(&schema["patternProperties"])?.into_owned();
                        patterns.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
                        let patterns = patterns.iter().map(repr).collect::<Vec<_>>();
                        let verb = if listed.len() == 1 { "does" } else { "do" };
                        format!(
                            "{} {verb} not match any of the regexes: {}",
                            listed.join(", "),
                            patterns.join(", ")
                        )
                    } else {
                        let verb = if listed.len() == 1 { "was" } else { "were" };
                        format!(
                            "Additional properties are not allowed ({} {verb} unexpected)",
                            listed.join(", ")
                        )
                    };
                    errors.push(error(message));
                }
            }
            "allOf" => {
                for (index, subschema) in py_iter(value)?.iter().enumerate() {
                    let schema_path = Some(PathItem::Index(index));
                    self.descend(
                        validator,
                        instance,
                        subschema,
                        None,
                        schema_path,
                        None,
                        errors,
                    )?;
                    if self.full(errors) {
                        break;
                    }
                }
            }
            "anyOf" => {
                let mut all = Vec::new();
                for (index, subschema) in py_iter(value)?.iter().enumerate() {
                    let found = self.collect(usize::MAX, |this, errs| {
                        let schema_path = Some(PathItem::Index(index));
                        this.descend(
                            validator,
                            instance,
                            subschema,
                            None,
                            schema_path,
                            None,
                            errs,
                        )
                    })?;
                    if found.is_empty() {
                        return Ok(());
                    }
                    all.extend(found);
                }
                let mut error = error(format!(
                    "{} is not valid under any of the given schemas",
                    inst()
                ));
                error.context = all;
                errors.push(error);
            }
            "oneOf" => {
                let subschemas = py_iter(value)?;
                let mut all = Vec::new();
                let mut first_valid = None;
                for (index, subschema) in subschemas.iter().enumerate() {
                    let found = self.collect(usize::MAX, |this, errs| {
                        let schema_path = Some(PathItem::Index(index));
                        this.descend(
                            validator,
                            instance,
                            subschema,
                            None,
                            schema_path,
                            None,
                            errs,
                        )
                    })?;
                    if found.is_empty() {
                        first_valid = Some(index);
                        break;
                    }
                    all.extend(found);
                }
                let Some(first) = first_valid else {
                    let mut error = error(format!(
                        "{} is not valid under any of the given schemas",
                        inst()
                    ));
                    error.context = all;
                    errors.push(error);
                    return Ok(());
                };
                let mut more_valid = Vec::new();
                for subschema in &subschemas[first + 1..] {
                    if self.is_valid(validator, subschema, instance)? {
                        more_valid.push(subschema);
                    }
                }
                if !more_valid.is_empty() {
                    more_valid.push(&subschemas[first]);
                    let reprs: Vec<String> = more_valid.iter().map(|s| repr(s)).collect();
                    errors.push(error(format!(
                        "{} is valid under each of {}",
                        inst(),
                        reprs.join(", ")
                    )));
                }
            }
            "const" => {
                if !json_equal(instance, value) {
                    errors.push(error(format!("{} was expected", repr(value))));
                }
            }
            "contains" if class.ignores_ref_siblings() => {
                let Value::Array(items) = instance else {
                    return Ok(());
                };
                let mut any = false;
                for item in items {
                    if self.is_valid(validator, value, item)? {
                        any = true;
                        break;
                    }
                }
                if !any {
                    errors.push(error(format!(
                        "None of {} are valid under the given schema",
                        inst()
                    )));
                }
            }
            "contains" => {
                let Value::Array(items) = instance else {
                    return Ok(());
                };
                let one = Value::from(1);
                let min_contains = schema.get("minContains").unwrap_or(&one);
                let len = Value::from(items.len());
                let max_contains = schema.get("maxContains").unwrap_or(&len);
                let contains = self.evolve(validator, value)?;
                let mut matches = 0usize;
                for item in items {
                    if self.is_valid_with(&contains, value, item)? {
                        matches += 1;
                        if compare(">", &Value::from(matches), max_contains)? {
                            let mut error = error(format!(
                                "Too many items match the given schema (expected at most {})",
                                py_str(max_contains)
                            ));
                            error.keyword = Some("maxContains".to_owned());
                            errors.push(error);
                            return Ok(());
                        }
                    }
                }
                if compare("<", &Value::from(matches), min_contains)? {
                    if matches == 0 {
                        errors.push(error(format!(
                            "{} does not contain items matching the given schema",
                            inst()
                        )));
                    } else {
                        let mut error = error(format!(
                            "Too few items match the given schema (expected at least {} but \
                             only {matches} matched)",
                            py_str(min_contains)
                        ));
                        error.keyword = Some("minContains".to_owned());
                        errors.push(error);
                    }
                }
            }
            "dependencies" => {
                let Some(object) = pyobj::as_dict(instance) else {
                    return Ok(());
                };
                for (property, dependency) in py_items(value)? {
                    if !object.contains_key(property) {
                        continue;
                    }
                    if dependency.is_array() {
                        self.dependent_required(object, property, dependency, errors)?;
                    } else {
                        let schema_path = Some(PathItem::Key(property.clone()));
                        self.descend(
                            validator,
                            instance,
                            dependency,
                            None,
                            schema_path,
                            None,
                            errors,
                        )?;
                    }
                    if self.full(errors) {
                        break;
                    }
                }
            }
            "dependentRequired" => {
                let Some(object) = pyobj::as_dict(instance) else {
                    return Ok(());
                };
                for (property, dependency) in py_items(value)? {
                    if object.contains_key(property) {
                        self.dependent_required(object, property, dependency, errors)?;
                        if self.full(errors) {
                            break;
                        }
                    }
                }
            }
            "dependentSchemas" => {
                if !instance.is_object() {
                    return Ok(());
                }
                for (property, dependency) in py_items(value)? {
                    if instance.get(property).is_none() {
                        continue;
                    }
                    let schema_path = Some(PathItem::Key(property.clone()));
                    self.descend(
                        validator,
                        instance,
                        dependency,
                        None,
                        schema_path,
                        None,
                        errors,
                    )?;
                    if self.full(errors) {
                        break;
                    }
                }
            }
            "enum" => {
                let options = py_iter(value)?;
                if options.iter().all(|option| !json_equal(option, instance)) {
                    errors.push(error(format!("{} is not one of {}", inst(), repr(value))));
                }
            }
            "exclusiveMaximum" | "exclusiveMinimum" | "maximum" | "minimum" => {
                if number(instance).is_none() {
                    return Ok(());
                }
                let (op, message, limit) = match keyword {
                    "maximum" if class == Class::Draft4 => {
                        if schema.get("exclusiveMaximum").is_some_and(truthy) {
                            (">=", "greater than or equal to", "maximum")
                        } else {
                            (">", "greater than", "maximum")
                        }
                    }
                    "minimum" if class == Class::Draft4 => {
                        if schema.get("exclusiveMinimum").is_some_and(truthy) {
                            ("<=", "less than or equal to", "minimum")
                        } else {
                            ("<", "less than", "minimum")
                        }
                    }
                    "maximum" => (">", "greater than", "maximum"),
                    "minimum" => ("<", "less than", "minimum"),
                    "exclusiveMaximum" => (">=", "greater than or equal to", "maximum"),
                    _ => ("<=", "less than or equal to", "minimum"),
                };
                if compare(op, instance, value)? {
                    errors.push(error(format!(
                        "{} is {message} the {limit} of {}",
                        inst(),
                        repr(value)
                    )));
                }
            }
            "format" => match value {
                Value::String(format) => {
                    let (outcome, cause) = formats::check_format_with_cause(
                        self.formats,
                        format,
                        instance,
                        &pyregex::check_regex_format,
                    );
                    match outcome {
                        FormatOutcome::Unknown | FormatOutcome::Valid => {}
                        FormatOutcome::Invalid(message) => {
                            let mut error = error(message);
                            error.cause = cause;
                            errors.push(error);
                        }
                        FormatOutcome::Crash(message) => return Err(Crash(message)),
                    }
                }
                // `format not in self.checkers` hashes the value.
                Value::Array(_) | Value::Object(_) => {
                    return Err(unhashable(value, pyobj::Hashed::DictKey));
                }
                _ => {}
            },
            "if" => {
                if self.is_valid(validator, value, instance)? {
                    if let Some(then) = schema.get("then") {
                        let schema_path = Some(PathItem::Key("then".to_owned()));
                        self.descend(validator, instance, then, None, schema_path, None, errors)?;
                    }
                } else if let Some(otherwise) = schema.get("else") {
                    let schema_path = Some(PathItem::Key("else".to_owned()));
                    self.descend(
                        validator,
                        instance,
                        otherwise,
                        None,
                        schema_path,
                        None,
                        errors,
                    )?;
                }
            }
            "items" => self.items(validator, value, schema, instance, errors)?,
            "maxItems" | "minItems" | "maxLength" | "minLength" | "maxProperties"
            | "minProperties" => {
                let len = match (keyword, instance) {
                    ("maxItems" | "minItems", Value::Array(items)) => items.len(),
                    ("maxLength" | "minLength", Value::String(s)) => s.chars().count(),
                    ("maxProperties" | "minProperties", Value::Object(map)) => map.len(),
                    _ => return Ok(()),
                };
                let len = Value::from(len);
                let is = |n: i128| {
                    pyobj::as_number(value).is_some_and(|v| {
                        num_cmp(&v, &Num::Int(n)) == Some(std::cmp::Ordering::Equal)
                    })
                };
                let message = if keyword.starts_with("max") {
                    if !compare(">", &len, value)? {
                        return Ok(());
                    }
                    match keyword {
                        _ if is(0) => "is expected to be empty",
                        "maxProperties" => "has too many properties",
                        _ => "is too long",
                    }
                } else {
                    if !compare("<", &len, value)? {
                        return Ok(());
                    }
                    match keyword {
                        _ if is(1) => "should be non-empty",
                        "minProperties" => "does not have enough properties",
                        _ => "is too short",
                    }
                };
                errors.push(error(format!("{} {message}", inst())));
            }
            "multipleOf" => {
                if let Some(x) = number(instance) {
                    if multiple_of(x, instance, value)? {
                        errors.push(error(format!(
                            "{} is not a multiple of {}",
                            inst(),
                            py_str(value)
                        )));
                    }
                }
            }
            "not" => {
                if self.is_valid(validator, value, instance)? {
                    errors.push(error(format!(
                        "{} should not be valid under {}",
                        inst(),
                        repr(value)
                    )));
                }
            }
            "pattern" => {
                if let Value::String(text) = instance {
                    if !self.search(value, text)? {
                        errors.push(error(format!("{} does not match {}", inst(), repr(value))));
                    }
                }
            }
            "patternProperties" => {
                let Some(object) = pyobj::as_dict(instance) else {
                    return Ok(());
                };
                for (pattern, subschema) in py_items(value)? {
                    for (key, child) in object {
                        if self.search(&Value::String(pattern.clone()), key)? {
                            let path = Some(PathItem::Key(key.clone()));
                            let schema_path = Some(PathItem::Key(pattern.clone()));
                            self.descend(
                                validator,
                                child,
                                subschema,
                                path,
                                schema_path,
                                None,
                                errors,
                            )?;
                            if self.full(errors) {
                                return Ok(());
                            }
                        }
                    }
                }
            }
            "prefixItems" => {
                let Value::Array(items) = instance else {
                    return Ok(());
                };
                let subschemas = py_iter(value)?;
                for (index, (item, subschema)) in items.iter().zip(subschemas.iter()).enumerate() {
                    let path = Some(PathItem::Index(index));
                    let schema_path = Some(PathItem::Index(index));
                    self.descend(validator, item, subschema, path, schema_path, None, errors)?;
                    if self.full(errors) {
                        break;
                    }
                }
            }
            "properties" => {
                let Some(object) = pyobj::as_dict(instance) else {
                    return Ok(());
                };
                for (property, subschema) in py_items(value)? {
                    if let Some(child) = object.get(property) {
                        let path = Some(PathItem::Key(property.clone()));
                        let schema_path = path.clone();
                        self.descend(validator, child, subschema, path, schema_path, None, errors)?;
                        if self.full(errors) {
                            break;
                        }
                    }
                }
            }
            "propertyNames" => {
                let Some(object) = pyobj::as_dict(instance) else {
                    return Ok(());
                };
                for property in object.keys() {
                    let name = Value::String(property.clone());
                    self.descend(validator, &name, value, None, None, None, errors)?;
                    if self.full(errors) {
                        break;
                    }
                }
            }
            "required" => {
                let Some(object) = pyobj::as_dict(instance) else {
                    return Ok(());
                };
                for property in py_iter(value)?.iter() {
                    if !contains_key(object, property)? {
                        errors.push(error(format!("{} is a required property", repr(property))));
                        if self.full(errors) {
                            break;
                        }
                    }
                }
            }
            "type" => {
                let types = match value {
                    Value::String(_) => Cow::Owned(vec![value.clone()]),
                    other => py_iter(other)?,
                };
                let mut matched = false;
                for name in types.iter() {
                    let known = match name {
                        Value::String(name) => class.is_type(instance, name),
                        // The type checker's map hashes it.
                        Value::Array(_) | Value::Object(_) => {
                            return Err(unhashable(name, pyobj::Hashed::Plain));
                        }
                        _ => None,
                    };
                    match known {
                        Some(true) => {
                            matched = true;
                            break;
                        }
                        Some(false) => {}
                        None => return Err(unknown_type(name, instance, schema)),
                    }
                }
                if !matched {
                    let reprs: Vec<String> = types.iter().map(repr).collect();
                    errors.push(error(format!(
                        "{} is not of type {}",
                        inst(),
                        reprs.join(", ")
                    )));
                }
            }
            "unevaluatedItems" => {
                let Value::Array(items) = instance else {
                    return Ok(());
                };
                let evaluated = self.evaluated_items_in(validator, instance, schema)?;
                let unevaluated: Vec<Value> = items
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| !evaluated.contains(index))
                    .map(|(_, item)| item.clone())
                    .collect();
                if !unevaluated.is_empty() {
                    errors.push(error(format!(
                        "Unevaluated items are not allowed ({} unexpected)",
                        extras_msg(&unevaluated)
                    )));
                }
            }
            "unevaluatedProperties" => {
                let Some(object) = pyobj::as_dict(instance) else {
                    return Ok(());
                };
                let evaluated = self.evaluated_properties_in(validator, instance, schema)?;
                let mut unevaluated = Vec::new();
                for (property, child) in object {
                    if evaluated.contains(property) {
                        continue;
                    }
                    let found = self.collect(usize::MAX, |this, errs| {
                        let path = Some(PathItem::Key(property.clone()));
                        let schema_path = path.clone();
                        this.descend(validator, child, value, path, schema_path, None, errs)
                    })?;
                    // One entry per error, as upstream appends in its loop over them.
                    unevaluated.extend(found.iter().map(|_| Value::String(property.clone())));
                }
                if !unevaluated.is_empty() {
                    let message = if value == &Value::Bool(false) {
                        unevaluated.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
                        format!(
                            "Unevaluated properties are not allowed ({} unexpected)",
                            extras_msg(&unevaluated)
                        )
                    } else {
                        format!(
                            "Unevaluated properties are not valid under the given schema ({} \
                             unevaluated and invalid)",
                            extras_msg(&unevaluated)
                        )
                    };
                    errors.push(error(message));
                }
            }
            "uniqueItems" if truthy(value) => {
                if let Value::Array(items) = instance {
                    let unique = items
                        .iter()
                        .enumerate()
                        .all(|(i, a)| items[i + 1..].iter().all(|b| !json_equal(a, b)));
                    if !unique {
                        errors.push(error(format!("{} has non-unique elements", inst())));
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// The `items` keyword of each draft.
    fn items(
        &mut self,
        validator: &Validator,
        value: &Value,
        schema: &Map<String, Value>,
        instance: &Value,
        errors: &mut Errors,
    ) -> Result<(), Crash> {
        let Value::Array(items) = instance else {
            return Ok(());
        };
        let mut prefix = 0;
        match validator.class {
            // `items_draft3_draft4`, `items_draft6_draft7_draft201909`: a list of schemas
            // validates the items position by position.
            Class::Draft4 | Class::Draft6 | Class::Draft7 | Class::Draft201909 => {
                let tuple = if validator.class == Class::Draft4 {
                    !value.is_object()
                } else {
                    value.is_array()
                };
                if tuple {
                    let subschemas = py_iter(value)?;
                    for (index, (item, subschema)) in
                        items.iter().zip(subschemas.iter()).enumerate()
                    {
                        let path = Some(PathItem::Index(index));
                        let schema_path = Some(PathItem::Index(index));
                        self.descend(validator, item, subschema, path, schema_path, None, errors)?;
                        if self.full(errors) {
                            break;
                        }
                    }
                    return Ok(());
                }
            }
            // 2020-12: `items` covers what `prefixItems` leaves.
            Class::Draft202012 | Class::Oas31 | Class::Oas32 => {
                prefix = schema.get("prefixItems").map_or(Ok(0), py_len)?;
                if items.len() <= prefix {
                    return Ok(());
                }
                if value == &Value::Bool(false) {
                    let extra = items.len() - prefix;
                    let rest = if extra == 1 {
                        repr(&items[prefix])
                    } else {
                        repr(&Value::Array(items[prefix..].to_vec()))
                    };
                    let noun = if prefix == 1 { "item" } else { "items" };
                    errors.push(error(format!(
                        "Expected at most {prefix} {noun} but found {extra} extra: {rest}"
                    )));
                    return Ok(());
                }
            }
        }
        for (index, item) in items.iter().enumerate().skip(prefix) {
            let path = Some(PathItem::Index(index));
            self.descend(validator, item, value, path, None, None, errors)?;
            if self.full(errors) {
                break;
            }
        }
        Ok(())
    }

    fn dependent_required(
        &mut self,
        object: &Map<String, Value>,
        property: &str,
        dependency: &Value,
        errors: &mut Errors,
    ) -> Result<(), Crash> {
        for each in py_iter(dependency)?.iter() {
            if !contains_key(object, each)? {
                errors.push(error(format!(
                    "{} is a dependency of {}",
                    repr(each),
                    repr_str(property)
                )));
                if self.full(errors) {
                    break;
                }
            }
        }
        Ok(())
    }

    /// `find_additional_properties`: keys in neither `properties` nor matching any
    /// `patternProperties` regex (joined with `|`).
    fn additional_properties<'o>(
        &mut self,
        object: &'o Map<String, Value>,
        schema: &Map<String, Value>,
    ) -> Result<Vec<&'o str>, Crash> {
        let empty = Value::Object(Map::new());
        let properties = schema.get("properties").unwrap_or(&empty);
        let patterns = py_join(schema.get("patternProperties").unwrap_or(&empty))?;
        let mut extras = Vec::new();
        for key in object.keys() {
            if contains(properties, key)? {
                continue;
            }
            if !patterns.is_empty() && self.search(&Value::String(patterns.clone()), key)? {
                continue;
            }
            extras.push(key.as_str());
        }
        Ok(extras)
    }

    /// `find_evaluated_item_indexes_by_schema` (the Draft 2019-09 variant for that class).
    fn evaluated_items(
        &mut self,
        validator: &Validator,
        instance: &Value,
        schema: &Value,
    ) -> Result<Vec<usize>, Crash> {
        match schema {
            Value::Bool(_) => Ok(Vec::new()),
            Value::Object(map) => {
                self.enter(1)?;
                let evaluated = self.evaluated_items_in(validator, instance, map)?;
                self.leave(1);
                Ok(evaluated)
            }
            other => Err(no_attribute(other, "get")),
        }
    }

    fn evaluated_items_in(
        &mut self,
        validator: &Validator,
        instance: &Value,
        map: &Map<String, Value>,
    ) -> Result<Vec<usize>, Crash> {
        let items = instance.as_array().map(Vec::as_slice).unwrap_or_default();
        let legacy = validator.class == Class::Draft201909;
        let all = || (0..items.len()).collect::<Vec<_>>();
        if !legacy && map.contains_key("items") {
            return Ok(all());
        }
        let mut evaluated = Vec::new();
        for reference in self.references(map, legacy) {
            let (evolved, target) = self.referenced(validator, &reference)?;
            evaluated.extend(self.evaluated_items(&evolved, instance, target)?);
        }
        if legacy {
            if let Some(declared) = map.get("items") {
                if map.contains_key("additionalItems") || declared.is_object() {
                    return Ok(all());
                }
                evaluated.extend(0..py_len(declared)?);
            }
        } else if let Some(prefix) = map.get("prefixItems") {
            evaluated.extend(0..py_len(prefix)?);
        }
        if let Some(condition) = map.get("if") {
            if self.is_valid(validator, condition, instance)? {
                evaluated.extend(self.evaluated_items(validator, instance, condition)?);
                if let Some(then) = map.get("then") {
                    evaluated.extend(self.evaluated_items(validator, instance, then)?);
                }
            } else if let Some(otherwise) = map.get("else") {
                evaluated.extend(self.evaluated_items(validator, instance, otherwise)?);
            }
        }
        for keyword in ["contains", "unevaluatedItems"] {
            if let Some(subschema) = map.get(keyword) {
                for (index, item) in items.iter().enumerate() {
                    if self.is_valid(validator, subschema, item)? {
                        evaluated.push(index);
                    }
                }
            }
        }
        for keyword in ["allOf", "oneOf", "anyOf"] {
            if let Some(subschemas) = map.get(keyword) {
                for subschema in py_iter(subschemas)?.iter() {
                    if self.descends_cleanly(validator, instance, subschema)? {
                        evaluated.extend(self.evaluated_items(validator, instance, subschema)?);
                    }
                }
            }
        }
        Ok(evaluated)
    }

    /// `find_evaluated_property_keys_by_schema` (the Draft 2019-09 variant for that class).
    fn evaluated_properties(
        &mut self,
        validator: &Validator,
        instance: &Value,
        schema: &Value,
    ) -> Result<Vec<String>, Crash> {
        match schema {
            Value::Bool(_) => Ok(Vec::new()),
            Value::Object(map) => {
                self.enter(1)?;
                let evaluated = self.evaluated_properties_in(validator, instance, map)?;
                self.leave(1);
                Ok(evaluated)
            }
            other => Err(no_attribute(other, "get")),
        }
    }

    fn evaluated_properties_in(
        &mut self,
        validator: &Validator,
        instance: &Value,
        map: &Map<String, Value>,
    ) -> Result<Vec<String>, Crash> {
        let empty = Map::new();
        let object = pyobj::as_dict(instance).unwrap_or(&empty);
        let legacy = validator.class == Class::Draft201909;
        let present = |properties: &Map<String, Value>| {
            properties
                .keys()
                .filter(|p| object.contains_key(*p))
                .cloned()
                .collect::<Vec<_>>()
        };
        let mut evaluated = Vec::new();
        for reference in self.references(map, legacy) {
            let (evolved, target) = self.referenced(validator, &reference)?;
            evaluated.extend(self.evaluated_properties(&evolved, instance, target)?);
        }
        if legacy {
            for keyword in [
                "properties",
                "additionalProperties",
                "unevaluatedProperties",
            ] {
                match map.get(keyword) {
                    Some(Value::Bool(true)) => evaluated.extend(object.keys().cloned()),
                    Some(Value::Object(properties)) => evaluated.extend(present(properties)),
                    _ => {}
                }
            }
        } else {
            if let Some(Value::Object(properties)) = map.get("properties") {
                evaluated.extend(present(properties));
            }
            for keyword in ["additionalProperties", "unevaluatedProperties"] {
                let Some(subschema) = map.get(keyword).filter(|s| !s.is_null()) else {
                    continue;
                };
                for (key, child) in object {
                    if self.descends_cleanly(validator, child, subschema)? {
                        evaluated.push(key.clone());
                    }
                }
            }
        }
        if let Some(patterns) = map.get("patternProperties") {
            let patterns = py_iter(patterns)?;
            for property in object.keys() {
                for pattern in patterns.iter() {
                    if self.search(pattern, property)? {
                        evaluated.push(property.clone());
                    }
                }
            }
        }
        if let Some(dependent) = map.get("dependentSchemas") {
            for (property, subschema) in py_items(dependent)? {
                if object.contains_key(property) {
                    evaluated.extend(self.evaluated_properties(validator, instance, subschema)?);
                }
            }
        }
        for keyword in ["allOf", "oneOf", "anyOf"] {
            if let Some(subschemas) = map.get(keyword) {
                for subschema in py_iter(subschemas)?.iter() {
                    if self.descends_cleanly(validator, instance, subschema)? {
                        evaluated
                            .extend(self.evaluated_properties(validator, instance, subschema)?);
                    }
                }
            }
        }
        if let Some(condition) = map.get("if") {
            if self.is_valid(validator, condition, instance)? {
                evaluated.extend(self.evaluated_properties(validator, instance, condition)?);
                if let Some(then) = map.get("then") {
                    evaluated.extend(self.evaluated_properties(validator, instance, then)?);
                }
            } else if let Some(otherwise) = map.get("else") {
                evaluated.extend(self.evaluated_properties(validator, instance, otherwise)?);
            }
        }
        Ok(evaluated)
    }

    /// The references `find_evaluated_*` follow first: `$ref`, then `$dynamicRef` (or,
    /// for Draft 2019-09, `$recursiveRef`, which always starts from `#`).
    fn references(&self, map: &Map<String, Value>, legacy: bool) -> Vec<Value> {
        let mut references = Vec::new();
        if let Some(reference) = map.get("$ref").filter(|r| !r.is_null()) {
            references.push(reference.clone());
        }
        if legacy {
            if map.contains_key("$recursiveRef") {
                references.push(Value::from("#"));
            }
        } else if let Some(reference) = map.get("$dynamicRef").filter(|r| !r.is_null()) {
            references.push(reference.clone());
        }
        references
    }
}

/// The keywords `descend`/`iter_errors` apply (`applicable_validators(schema)`): the
/// mapping, and whether only its `$ref` counts.
fn keywords(class: Class, schema: &Value) -> Result<(&Map<String, Value>, bool), Crash> {
    let ref_only = class.ignores_ref_siblings();
    let Value::Object(map) = schema else {
        return Err(no_attribute(schema, if ref_only { "get" } else { "items" }));
    };
    Ok((
        map,
        ref_only && map.get("$ref").is_some_and(|r| !r.is_null()),
    ))
}

fn error(message: String) -> ValueError {
    ValueError::new(message)
}

fn false_schema_error(instance: &Value) -> ValueError {
    let mut error = ValueError::new(format!("False schema does not allow {}", repr(instance)));
    error.keyword = Some(String::new());
    error.located = true;
    error.schema = Value::Bool(false);
    error.instance = instance.clone();
    error
}

/// `extras_msg(extras)`: the reprs and the verb.
fn extras_msg(extras: &[Value]) -> String {
    let verb = if extras.len() == 1 { "was" } else { "were" };
    let reprs: Vec<String> = extras.iter().map(repr).collect();
    format!("{} {verb}", reprs.join(", "))
}

/// `left <op> right` for a number on the left, as Python compares it.
fn compare(op: &str, left: &Value, right: &Value) -> Result<bool, Crash> {
    let (Some(a), Some(b)) = (pyobj::as_number(left), pyobj::as_number(right)) else {
        return Err(unsupported_comparison(op, left, right));
    };
    let ordering = num_cmp(&a, &b);
    Ok(match op {
        "<" => ordering == Some(std::cmp::Ordering::Less),
        "<=" => matches!(
            ordering,
            Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
        ),
        ">" => ordering == Some(std::cmp::Ordering::Greater),
        _ => matches!(
            ordering,
            Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
        ),
    })
}

/// `iter(value)`: list items, dict keys or string characters.
fn py_iter(value: &Value) -> Result<Cow<'_, [Value]>, Crash> {
    match value {
        Value::Array(items) => Ok(Cow::Borrowed(items)),
        other => pyobj::iterate(other).map(Cow::Owned).map_err(Crash),
    }
}

/// `value.items()`, which only mappings have.
fn py_items(value: &Value) -> Result<&Map<String, Value>, Crash> {
    value
        .as_object()
        .ok_or_else(|| no_attribute(value, "items"))
}

/// `len(value)`.
fn py_len(value: &Value) -> Result<usize, Crash> {
    match value {
        Value::Array(items) => Ok(items.len()),
        Value::Object(map) if foreign(value).is_none() => Ok(map.len()),
        Value::String(s) => Ok(s.chars().count()),
        other => Err(Crash(format!(
            "object of type '{}' has no len()",
            python_type_name(other)
        ))),
    }
}

/// `"|".join(value)`.
fn py_join(value: &Value) -> Result<String, Crash> {
    let parts = match value {
        Value::Object(map) if foreign(value).is_none() => map.keys().cloned().collect::<Vec<_>>(),
        Value::String(s) => s.chars().map(String::from).collect(),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(i, item)| match item {
                Value::String(s) => Ok(s.clone()),
                other => Err(Crash(format!(
                    "sequence item {i}: expected str instance, {} found",
                    python_type_name(other)
                ))),
            })
            .collect::<Result<_, _>>()?,
        _ => return Err(Crash("can only join an iterable".to_owned())),
    };
    Ok(parts.join("|"))
}

/// `key in container` for a string key.
fn contains(container: &Value, key: &str) -> Result<bool, Crash> {
    match container {
        Value::Object(map) if foreign(container).is_none() => Ok(map.contains_key(key)),
        Value::Array(items) => Ok(items.iter().any(|item| item.as_str() == Some(key))),
        Value::String(s) => Ok(s.contains(key)),
        other => Err(not_iterable_argument(other)),
    }
}

/// `item in mapping` for a dict loaded from JSON: only strings can be keys.
fn contains_key(object: &Map<String, Value>, item: &Value) -> Result<bool, Crash> {
    match item {
        Value::String(key) => Ok(object.contains_key(key)),
        Value::Array(_) | Value::Object(_) => Err(unhashable(item, pyobj::Hashed::DictKey)),
        _ => Ok(false),
    }
}

fn no_attribute(value: &Value, attribute: &str) -> Crash {
    Crash(format!(
        "'{}' object has no attribute '{attribute}'",
        python_type_name(value)
    ))
}

fn not_iterable_argument(value: &Value) -> Crash {
    Crash(pyobj::not_a_container(value))
}

fn unhashable(value: &Value, hashed: pyobj::Hashed) -> Crash {
    Crash(pyobj::unhashable(value, hashed))
}

/// `UnknownType(type, instance, schema)`, rendered like jsonschema.
pub(crate) fn unknown_type(name: &Value, instance: &Value, schema: &Map<String, Value>) -> Crash {
    let pretty = |value: &Value| pformat(value, 72, false).replace('\n', "\n    ");
    Crash(format!(
        "Unknown type {} for validator with schema:\n    {}\n\nWhile checking instance:\n    {}",
        repr(name),
        pretty(&Value::Object(schema.clone())),
        pretty(instance)
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn errors(spec_schema: &Value, instance: &Value) -> Result<Vec<String>, Crash> {
        // Validation runs on a large stack (see `crate::validate`).
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(64 << 20)
                .spawn_scoped(scope, || {
                    let docs = Documents::new(spec_schema, "", None);
                    let mut regexes = HashMap::new();
                    let found = Evaluator::run(
                        &docs,
                        FormatSet::Oas31,
                        &mut regexes,
                        docs.root(),
                        instance,
                        usize::MAX,
                    )?;
                    Ok(found.into_iter().map(|e| e.message).collect())
                })
                .expect("test thread spawns")
                .join()
                .expect("validation does not panic")
        })
    }

    #[test]
    fn float_multiple_of_follows_python_division() {
        let schema = json!({"type": "number", "multipleOf": 0.1});
        assert_eq!(
            errors(&schema, &json!(0.3)).unwrap(),
            ["0.3 is not a multiple of 0.1"]
        );
        assert!(errors(&schema, &json!(0.5)).unwrap().is_empty());
        let huge = json!({"multipleOf": 1e-300});
        assert_eq!(
            errors(&huge, &json!(1e300)).unwrap(),
            ["1e+300 is not a multiple of 1e-300"]
        );
    }

    #[test]
    fn malformed_keywords_fail_when_applied() {
        let schema = json!({"type": "string", "maximum": "z"});
        // The type error comes first; `maximum` only applies to numbers.
        assert_eq!(
            errors(&schema, &json!(5)).unwrap_err(),
            Crash("'>' not supported between instances of 'int' and 'str'".to_owned())
        );
        assert_eq!(errors(&schema, &json!("a")).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn messages_are_jsonschemas() {
        let cases = [
            (
                json!({"contains": {"type": "string"}, "minContains": 2}),
                json!(["a"]),
                "Too few items match the given schema (expected at least 2 but only 1 matched)",
            ),
            (
                json!({"contains": false}),
                json!([1]),
                "[1] does not contain items matching the given schema",
            ),
            (
                json!({"dependentRequired": {"a": ["b"]}}),
                json!({"a": 1}),
                "'b' is a dependency of 'a'",
            ),
            (
                json!({"propertyNames": {"maxLength": 1}}),
                json!({"ab": 1}),
                "'ab' is too long",
            ),
            (
                json!({"unevaluatedItems": false}),
                json!([1]),
                "Unevaluated items are not allowed (1 was unexpected)",
            ),
            (
                json!({"additionalProperties": false}),
                json!({"b": 1, "a": 2}),
                "Additional properties are not allowed ('a', 'b' were unexpected)",
            ),
            (
                json!({"oneOf": [{"type": "integer"}, {"type": "number"}, {"minimum": 0}]}),
                json!(1),
                "1 is valid under each of {'type': 'number'}, {'minimum': 0}, {'type': 'integer'}",
            ),
        ];
        for (schema, instance, message) in cases {
            assert_eq!(errors(&schema, &instance).unwrap(), [message], "{schema}");
        }
    }

    #[test]
    fn nested_dialects_use_their_own_keywords() {
        // Draft 4's boolean `exclusiveMaximum` modifies `maximum`.
        let schema = json!({"properties": {"a": {
            "$schema": "http://json-schema.org/draft-04/schema#",
            "maximum": 1,
            "exclusiveMaximum": true,
        }}});
        assert_eq!(
            errors(&schema, &json!({"a": 1})).unwrap(),
            ["1 is greater than or equal to the maximum of 1"]
        );
    }

    #[test]
    fn reference_cycles_hit_the_recursion_limit() {
        let schema = json!({"$ref": "#"});
        assert_eq!(errors(&schema, &json!(1)).unwrap_err(), Crash::recursion());
    }

    #[test]
    fn first_error_stops_evaluation() {
        // `maximum` would raise, but the consumer stops at the `type` error before it.
        let schema = json!({"type": "string", "maximum": "z"});
        let docs = Documents::new(&schema, "", None);
        let mut regexes = HashMap::new();
        let first = Evaluator::run(
            &docs,
            FormatSet::Oas31,
            &mut regexes,
            docs.root(),
            &json!(5),
            1,
        )
        .unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].message, "5 is not of type 'string'");
    }
}
