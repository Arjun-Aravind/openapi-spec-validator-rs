//! Validation of `default` values against their schema (upstream's `ValueValidator`).
//!
//! Swagger 2.0 and OpenAPI 3.0 use openapi-schema-validator's `OAS30Validator`, a
//! Draft 4 variant with OpenAPI keywords; it is ported here keyword by keyword so
//! that iteration order, messages and edge-case exceptions match. OpenAPI 3.1 and 3.2
//! use `OAS31Validator` / `OAS32Validator`, ported in [`crate::pyjsonschema`].

use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};

use serde_json::{Map, Value};

use crate::document::{Crash, Documents, LookupError, Node};
use crate::error::{SchemaErrorDetails, ValidationError};
use crate::formats::{self, FormatOutcome, FormatSet};
use crate::meta;
use crate::pyjsonschema::{Evaluator, unknown_type};
use crate::pyobj::{self, Num, json_equal, num_cmp, truthy};
use crate::pyregex::{self, PyRegex};
use crate::pyrepr::{self, PathItem, repr, repr_float, repr_str};
use crate::version::{SpecVersion, python_type_name};

/// Python frames upstream can still use when it starts validating a `default`: its
/// recursion limit (1000) minus the frames of the command line and the walk above it.
/// Measured with `$ref` chains and recursive schemas under a component's `default`.
const MAX_FRAMES: usize = 982;

pub(crate) struct DefaultValidator<'d, 'a> {
    docs: &'d Documents<'a>,
    version: SpecVersion,
    regexes: HashMap<String, Result<PyRegex, String>>,
}

impl<'d, 'a> DefaultValidator<'d, 'a> {
    pub fn new(docs: &'d Documents<'a>, version: SpecVersion) -> Self {
        Self {
            docs,
            version,
            regexes: HashMap::new(),
        }
    }

    /// The errors of `value` against `schema`, or the exception upstream would raise.
    /// Upstream iterates lazily: with `first_only`, nothing after the first error is
    /// evaluated, so an exception further on never happens.
    pub fn validate(
        &mut self,
        schema: Node<'d>,
        value: &'d Value,
        first_only: bool,
    ) -> Result<Vec<ValidationError>, Crash> {
        let limit = if first_only { 1 } else { usize::MAX };
        let errors = match self.version {
            SpecVersion::V2_0 | SpecVersion::V3_0 => {
                let mut errors = Vec::new();
                let mut eval = Oas30 {
                    docs: self.docs,
                    regexes: &mut self.regexes,
                    frames: 0,
                    limit,
                };
                eval.descend(value, schema.value, schema.doc, &mut errors)?;
                errors
            }
            SpecVersion::V3_1 | SpecVersion::V3_2 => {
                let formats = if self.version == SpecVersion::V3_1 {
                    FormatSet::Oas31
                } else {
                    FormatSet::Oas32
                };
                Evaluator::run(self.docs, formats, &mut self.regexes, schema, value, limit)?
            }
        };
        Ok(errors
            .into_iter()
            .map(ValueError::into_validation_error)
            .collect())
    }
}

/// An error of a `default` value, built up like jsonschema's `ValidationError`: the
/// keyword function creates it, and each enclosing `descend` adds what it knows.
pub(crate) struct ValueError {
    pub message: String,
    /// `validator`: `None` while unset; filled in by the `descend` that applied the
    /// keyword. Errors from a `false` schema keep `Some("")`, i.e. `validator=None`.
    pub keyword: Option<String>,
    /// Whether `schema` and `instance` are set (by the keyword's `descend`).
    pub located: bool,
    /// Set for an error from a `false` schema until it leaves the `descend` that met that
    /// schema: Python's `descend` returns before adding its path segments.
    pub bare_false: bool,
    pub schema_path: VecDeque<PathItem>,
    pub instance_path: VecDeque<PathItem>,
    pub schema: Value,
    pub instance: Value,
    pub cause: Option<String>,
    pub context: Vec<ValueError>,
}

impl ValueError {
    pub(crate) fn new(message: String) -> Self {
        Self {
            message,
            keyword: None,
            located: false,
            bare_false: false,
            schema_path: VecDeque::new(),
            instance_path: VecDeque::new(),
            schema: Value::Null,
            instance: Value::Null,
            cause: None,
            context: Vec::new(),
        }
    }

    fn into_validation_error(self) -> ValidationError {
        let context = self
            .context
            .into_iter()
            .map(ValueError::into_validation_error)
            .collect();
        ValidationError::from_schema(
            self.message,
            SchemaErrorDetails {
                keyword: self.keyword.unwrap_or_default(),
                schema_path: self.schema_path.into(),
                schema: self.schema,
                instance_path: self.instance_path.into(),
                instance: self.instance,
                cause: self.cause,
                context,
            },
        )
    }
}

/// Port of openapi-schema-validator's `OAS30Validator`.
struct Oas30<'d, 'a, 'r> {
    docs: &'d Documents<'a>,
    regexes: &'r mut HashMap<String, Result<PyRegex, String>>,
    /// Python frames in use, to reproduce `RecursionError`.
    frames: usize,
    /// How many errors the innermost consumer takes before it stops iterating
    /// (see [`DefaultValidator::validate`]).
    limit: usize,
}

impl<'d> Oas30<'d, '_, '_> {
    fn enter(&mut self, frames: usize) -> Result<(), Crash> {
        crate::context::check();
        self.frames += frames;
        if self.frames > MAX_FRAMES {
            return Err(Crash::recursion());
        }
        Ok(())
    }

    fn full(&self, errors: &[ValueError]) -> bool {
        errors.len() >= self.limit
    }

    /// The errors of `run`, for a consumer that takes at most `limit` of them.
    fn collect(
        &mut self,
        limit: usize,
        run: impl FnOnce(&mut Self, &mut Vec<ValueError>) -> Result<(), Crash>,
    ) -> Result<Vec<ValueError>, Crash> {
        let outer = std::mem::replace(&mut self.limit, limit);
        let mut errors = Vec::new();
        let result = run(self, &mut errors);
        self.limit = outer;
        result.map(|()| errors)
    }

    /// `Validator.descend`/`iter_errors`: applies every known keyword of `schema`, in
    /// schema order (OAS30Validator applies `$ref` siblings too).
    fn descend(
        &mut self,
        instance: &'d Value,
        schema: &'d Value,
        doc: usize,
        errors: &mut Vec<ValueError>,
    ) -> Result<(), Crash> {
        let map = match schema {
            Value::Bool(true) => return Ok(()),
            Value::Bool(false) => {
                let mut error =
                    ValueError::new(format!("False schema does not allow {}", repr(instance)));
                error.keyword = Some(String::new());
                error.located = true;
                error.bare_false = true;
                error.schema = schema.clone();
                error.instance = instance.clone();
                errors.push(error);
                return Ok(());
            }
            Value::Object(map) => map,
            other => return Err(not_a_schema(other)),
        };
        self.enter(1)?;
        for (keyword, value) in map {
            if self.full(errors) {
                break;
            }
            let start = errors.len();
            self.enter(1)?;
            self.keyword(keyword, value, instance, map, doc, errors)?;
            self.frames -= 1;
            for error in &mut errors[start..] {
                if error.keyword.is_none() {
                    error.keyword = Some(keyword.clone());
                }
                if !error.located {
                    error.located = true;
                    error.schema = schema.clone();
                    error.instance = instance.clone();
                }
                if keyword != "$ref" {
                    error.schema_path.push_front(PathItem::Key(keyword.clone()));
                }
            }
        }
        self.frames -= 1;
        Ok(())
    }

    /// `validator.evolve(schema=schema).is_valid(instance)`: stops at the first error.
    fn is_valid(
        &mut self,
        instance: &'d Value,
        schema: &'d Value,
        doc: usize,
    ) -> Result<bool, Crash> {
        self.enter(1)?;
        let errors = self.collect(1, |this, errors| {
            this.descend(instance, schema, doc, errors)
        })?;
        self.frames -= 1;
        Ok(errors.is_empty())
    }

    fn error(message: String) -> ValueError {
        ValueError::new(message)
    }

    /// `TypeChecker.is_type` of the OAS 3.0 type checker: Draft 4's types without `null`
    /// (OAS 3.0 has `nullable` instead), so `type: 'null'` raises `UnknownType`.
    fn is_type(instance: &Value, name: &str, schema: &Map<String, Value>) -> Result<bool, Crash> {
        meta::is_draft4_type(instance, name)
            .filter(|_| name != "null")
            .ok_or_else(|| unknown_type(&Value::String(name.to_owned()), instance, schema))
    }

    #[allow(clippy::too_many_lines)]
    fn keyword(
        &mut self,
        keyword: &str,
        value: &'d Value,
        instance: &'d Value,
        schema: &'d Map<String, Value>,
        doc: usize,
        errors: &mut Vec<ValueError>,
    ) -> Result<(), Crash> {
        let inst = || repr(instance);
        match keyword {
            "multipleOf" => {
                let Some(x) = number(instance) else {
                    return Ok(());
                };
                let failed = multiple_of(x, instance, value)?;
                if failed {
                    let divisor = match value {
                        Value::Number(n) if n.is_f64() => {
                            repr_float(n.as_f64().unwrap_or_default())
                        }
                        _ => repr(value),
                    };
                    errors.push(Self::error(format!(
                        "{} is not a multiple of {divisor}",
                        inst()
                    )));
                }
            }
            "maximum" | "minimum" => {
                let Some(x) = number(instance) else {
                    return Ok(());
                };
                let Some(limit) = pyobj::as_number(value) else {
                    let op = if keyword == "maximum" { ">" } else { "<" };
                    return Err(unsupported_comparison(op, instance, value));
                };
                let exclusive_key = if keyword == "maximum" {
                    "exclusiveMaximum"
                } else {
                    "exclusiveMinimum"
                };
                let exclusive = schema.get(exclusive_key).is_some_and(truthy);
                let ordering = num_cmp(&x, &limit);
                let (failed, cmp) = match (keyword, exclusive) {
                    ("maximum", true) => (
                        matches!(ordering, Some(Ordering::Greater | Ordering::Equal)),
                        "greater than or equal to",
                    ),
                    ("maximum", false) => (ordering == Some(Ordering::Greater), "greater than"),
                    (_, true) => (
                        matches!(ordering, Some(Ordering::Less | Ordering::Equal)),
                        "less than or equal to",
                    ),
                    (_, false) => (ordering == Some(Ordering::Less), "less than"),
                };
                if failed {
                    errors.push(Self::error(format!(
                        "{} is {cmp} the {keyword} of {}",
                        inst(),
                        repr(value)
                    )));
                }
            }
            "maxLength" | "minLength" => {
                let Value::String(s) = instance else {
                    return Ok(());
                };
                let len = s.chars().count();
                self.length_check(keyword, len, value, instance, errors)?;
            }
            "maxItems" | "minItems" => {
                let Value::Array(items) = instance else {
                    return Ok(());
                };
                self.length_check(keyword, items.len(), value, instance, errors)?;
            }
            "maxProperties" | "minProperties" => {
                let Some(map) = pyobj::as_dict(instance) else {
                    return Ok(());
                };
                self.length_check(keyword, map.len(), value, instance, errors)?;
            }
            "pattern" => {
                let Value::String(text) = instance else {
                    return Ok(());
                };
                let Value::String(pattern) = value else {
                    return Err(Crash(
                        "first argument must be string or compiled pattern".to_owned(),
                    ));
                };
                let regex = self.regexes.entry(pattern.clone()).or_insert_with(|| {
                    PyRegex::compile(pattern).map_err(|e| match e.pos {
                        Some(pos) => format!("{} at position {pos}", e.msg),
                        None => e.msg,
                    })
                });
                let regex = regex.as_ref().map_err(|message| Crash(message.clone()))?;
                if !regex.search(text).map_err(Crash)? {
                    errors.push(Self::error(format!(
                        "{} does not match {}",
                        inst(),
                        repr_str(pattern)
                    )));
                }
            }
            "uniqueItems" => {
                if truthy(value) {
                    if let Value::Array(items) = instance {
                        let unique = items
                            .iter()
                            .enumerate()
                            .all(|(i, a)| items[i + 1..].iter().all(|b| !json_equal(a, b)));
                        if !unique {
                            errors.push(Self::error(format!("{} has non-unique elements", inst())));
                        }
                    }
                }
            }
            "enum" => {
                let options = pyobj::iterate(value).map_err(Crash)?;
                if options.iter().all(|option| !json_equal(option, instance)) {
                    errors.push(Self::error(format!(
                        "{} is not one of {}",
                        inst(),
                        repr(value)
                    )));
                }
            }
            "type" => {
                let Value::String(name) = value else {
                    // The type checker's map hashes it.
                    return Err(Crash(pyobj::unhashable(value, pyobj::Hashed::Plain)));
                };
                if instance.is_null() {
                    if schema.get("nullable") == Some(&Value::Bool(true)) {
                        return Ok(());
                    }
                    errors.push(Self::error("None for not nullable".to_owned()));
                    // A generator upstream: with the first error taken, the type name is
                    // never checked (so `type: 'null'` only raises `UnknownType` when every
                    // error is collected).
                    if self.full(errors) {
                        return Ok(());
                    }
                }
                // "Pragmatic": `bytes` pass as a string with `format: binary`.
                let binary_bytes = name == "string"
                    && schema.get("format").and_then(Value::as_str) == Some("binary")
                    && pyrepr::foreign(instance).is_some_and(|o| o.type_name == "bytes");
                if binary_bytes {
                    return Ok(());
                }
                if !Self::is_type(instance, name, schema)? {
                    errors.push(Self::error(format!(
                        "{} is not of type {}",
                        inst(),
                        repr_str(name)
                    )));
                }
            }
            "allOf" | "anyOf" | "oneOf" => {
                if schema.contains_key("discriminator") {
                    self.discriminator(instance, schema, doc, errors)?;
                } else {
                    let subschemas = value.as_array().map(Vec::as_slice).unwrap_or_default();
                    match keyword {
                        "allOf" => {
                            for (index, subschema) in subschemas.iter().enumerate() {
                                let start = errors.len();
                                self.descend(instance, subschema, doc, errors)?;
                                prefix(&mut errors[start..], None, Some(PathItem::Index(index)));
                                if self.full(errors) {
                                    break;
                                }
                            }
                        }
                        "anyOf" => {
                            let mut all = Vec::new();
                            let mut matched = false;
                            for (index, subschema) in subschemas.iter().enumerate() {
                                let mut errs = self.collect(usize::MAX, |this, errs| {
                                    this.descend(instance, subschema, doc, errs)
                                })?;
                                if errs.is_empty() {
                                    matched = true;
                                    break;
                                }
                                prefix(&mut errs, None, Some(PathItem::Index(index)));
                                all.extend(errs);
                            }
                            if !matched {
                                let mut error = Self::error(format!(
                                    "{} is not valid under any of the given schemas",
                                    inst()
                                ));
                                error.context = all;
                                errors.push(error);
                            }
                        }
                        _ => {
                            let mut all = Vec::new();
                            let mut first_valid = None;
                            for (index, subschema) in subschemas.iter().enumerate() {
                                let mut errs = self.collect(usize::MAX, |this, errs| {
                                    this.descend(instance, subschema, doc, errs)
                                })?;
                                if errs.is_empty() {
                                    first_valid = Some(index);
                                    break;
                                }
                                prefix(&mut errs, None, Some(PathItem::Index(index)));
                                all.extend(errs);
                            }
                            match first_valid {
                                None => {
                                    let mut error = Self::error(format!(
                                        "{} is not valid under any of the given schemas",
                                        inst()
                                    ));
                                    error.context = all;
                                    errors.push(error);
                                }
                                Some(first) => {
                                    let mut more_valid = Vec::new();
                                    for subschema in &subschemas[first + 1..] {
                                        if self.is_valid(instance, subschema, doc)? {
                                            more_valid.push(subschema);
                                        }
                                    }
                                    if !more_valid.is_empty() {
                                        more_valid.push(&subschemas[first]);
                                        let reprs: Vec<String> =
                                            more_valid.iter().map(|s| repr(s)).collect();
                                        errors.push(Self::error(format!(
                                            "{} is valid under each of {}",
                                            inst(),
                                            reprs.join(", ")
                                        )));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            "not" => {
                if self.is_valid(instance, value, doc)? {
                    errors.push(Self::error(format!(
                        "{} should not be valid under {}",
                        inst(),
                        repr(value)
                    )));
                }
            }
            "items" => {
                let Value::Array(items) = instance else {
                    return Ok(());
                };
                for (index, item) in items.iter().enumerate() {
                    let start = errors.len();
                    self.descend(item, value, doc, errors)?;
                    prefix(&mut errors[start..], Some(PathItem::Index(index)), None);
                    if self.full(errors) {
                        break;
                    }
                }
            }
            "properties" => {
                let Some(object) = pyobj::as_dict(instance) else {
                    return Ok(());
                };
                let Value::Object(properties) = value else {
                    return Err(Crash(format!(
                        "'{}' object has no attribute 'items'",
                        python_type_name(value)
                    )));
                };
                for (property, subschema) in properties {
                    if let Some(child) = object.get(property) {
                        let start = errors.len();
                        self.descend(child, subschema, doc, errors)?;
                        prefix(
                            &mut errors[start..],
                            Some(PathItem::Key(property.clone())),
                            Some(PathItem::Key(property.clone())),
                        );
                        if self.full(errors) {
                            break;
                        }
                    }
                }
            }
            "required" => {
                let Some(object) = pyobj::as_dict(instance) else {
                    return Ok(());
                };
                for property in pyobj::iterate(value).map_err(Crash)? {
                    let present = property.as_str().is_some_and(|p| object.contains_key(p));
                    if present {
                        continue;
                    }
                    // `schema.get("properties", {}).get(property)`, evaluated lazily.
                    let properties = match schema.get("properties") {
                        None => None,
                        Some(Value::Object(map)) => Some(map),
                        Some(other) => {
                            return Err(Crash(format!(
                                "'{}' object has no attribute 'get'",
                                python_type_name(other)
                            )));
                        }
                    };
                    let prop_schema = property
                        .as_str()
                        .and_then(|p| properties.and_then(|props| props.get(p)));
                    if let Some(prop_schema) = prop_schema.filter(|s| truthy(s)) {
                        let Value::Object(prop_schema) = prop_schema else {
                            return Err(Crash(format!(
                                "'{}' object has no attribute 'get'",
                                python_type_name(prop_schema)
                            )));
                        };
                        let read_only = prop_schema.get("readOnly").is_some_and(truthy);
                        let write_only = prop_schema.get("writeOnly").is_some_and(truthy);
                        if read_only || write_only {
                            continue;
                        }
                    }
                    errors.push(Self::error(format!(
                        "{} is a required property",
                        repr(&property)
                    )));
                    if self.full(errors) {
                        break;
                    }
                }
            }
            "additionalProperties" => {
                let Some(object) = pyobj::as_dict(instance) else {
                    return Ok(());
                };
                let extras = self.additional_properties(object, schema)?;
                if extras.is_empty() {
                    return Ok(());
                }
                if value.is_object() {
                    for extra in extras {
                        let start = errors.len();
                        self.descend(&object[extra], value, doc, errors)?;
                        prefix(
                            &mut errors[start..],
                            Some(PathItem::Key(extra.to_owned())),
                            None,
                        );
                        if self.full(errors) {
                            break;
                        }
                    }
                } else if value == &Value::Bool(false) {
                    let verb = if extras.len() == 1 { "was" } else { "were" };
                    let listed: Vec<String> = extras.iter().map(|e| repr_str(e)).collect();
                    errors.push(Self::error(format!(
                        "Additional properties are not allowed ({} {verb} unexpected)",
                        listed.join(", ")
                    )));
                }
            }
            "format" => {
                if instance.is_null() {
                    return Ok(());
                }
                let Value::String(format) = value else {
                    return Ok(());
                };
                let (outcome, cause) = formats::check_format_with_cause(
                    FormatSet::Oas30,
                    format,
                    instance,
                    &pyregex::check_regex_format,
                );
                match outcome {
                    FormatOutcome::Unknown | FormatOutcome::Valid => {}
                    FormatOutcome::Invalid(message) => {
                        let mut error = Self::error(message);
                        error.cause = cause;
                        errors.push(error);
                    }
                    FormatOutcome::Crash(message) => return Err(Crash(message)),
                }
            }
            "$ref" => {
                // `Resolver.lookup` starts with `ref.startswith("#")`.
                let Value::String(reference) = value else {
                    return Err(Crash(format!(
                        "'{}' object has no attribute 'startswith'",
                        python_type_name(value)
                    )));
                };
                let target = self
                    .docs
                    .lookup_node(doc, reference)
                    .map_err(LookupError::wrapped)?;
                self.descend(instance, target.value, target.doc, errors)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn length_check(
        &mut self,
        keyword: &str,
        len: usize,
        limit: &Value,
        instance: &Value,
        errors: &mut Vec<ValueError>,
    ) -> Result<(), Crash> {
        let Some(limit_num) = pyobj::as_number(limit) else {
            let op = if keyword.starts_with("max") { ">" } else { "<" };
            return Err(unsupported_comparison(op, &Value::from(len), limit));
        };
        #[allow(clippy::cast_possible_wrap)]
        let len_num = Num::Int(len as i128);
        let ordering = num_cmp(&len_num, &limit_num);
        let is = |n: i128| num_cmp(&limit_num, &Num::Int(n)) == Some(Ordering::Equal);
        let message = if keyword.starts_with("max") {
            if ordering != Some(Ordering::Greater) {
                return Ok(());
            }
            match keyword {
                "maxProperties" if is(0) => "is expected to be empty",
                "maxProperties" => "has too many properties",
                _ if is(0) => "is expected to be empty",
                _ => "is too long",
            }
        } else {
            if ordering != Some(Ordering::Less) {
                return Ok(());
            }
            match keyword {
                "minProperties" if is(1) => "should be non-empty",
                "minProperties" => "does not have enough properties",
                _ if is(1) => "should be non-empty",
                _ => "is too short",
            }
        };
        errors.push(Self::error(format!("{} {message}", repr(instance))));
        Ok(())
    }

    /// `find_additional_properties`: keys not in `properties` nor matching any
    /// `patternProperties` regex (joined with `|`, searched with Python `re`).
    fn additional_properties<'o>(
        &mut self,
        object: &'o Map<String, Value>,
        schema: &Map<String, Value>,
    ) -> Result<Vec<&'o str>, Crash> {
        let properties = schema.get("properties").and_then(Value::as_object);
        let patterns = schema
            .get("patternProperties")
            .and_then(Value::as_object)
            .map(|map| map.keys().cloned().collect::<Vec<_>>().join("|"))
            .unwrap_or_default();
        let regex = if patterns.is_empty() {
            None
        } else {
            Some(PyRegex::compile(&patterns).map_err(|e| Crash(e.python_str(&patterns)))?)
        };
        let mut extras = Vec::new();
        for key in object.keys() {
            if properties.is_some_and(|p| p.contains_key(key)) {
                continue;
            }
            if let Some(regex) = &regex {
                if regex.search(key).map_err(Crash)? {
                    continue;
                }
            }
            extras.push(key.as_str());
        }
        Ok(extras)
    }

    /// `handle_discriminator` for `allOf`/`anyOf`/`oneOf` next to a `discriminator`.
    fn discriminator(
        &mut self,
        instance: &'d Value,
        schema: &'d Map<String, Value>,
        doc: usize,
        errors: &mut Vec<ValueError>,
    ) -> Result<(), Crash> {
        let discriminator = &schema["discriminator"];
        let Value::Object(discriminator) = discriminator else {
            // `discriminator["propertyName"]`
            return Err(Crash(match discriminator {
                Value::String(_) => "string indices must be integers, not 'str'".to_owned(),
                Value::Array(_) => "list indices must be integers or slices, not str".to_owned(),
                other => format!("'{}' object is not subscriptable", python_type_name(other)),
            }));
        };
        let prop_name = discriminator
            .get("propertyName")
            .ok_or_else(|| Crash::key_error("propertyName"))?;
        let Some(object) = pyobj::as_dict(instance) else {
            errors.push(Self::error(format!(
                "{} is not of type 'object'",
                repr(instance)
            )));
            return Ok(());
        };
        pyobj::check_hashable(prop_name, pyobj::Hashed::DictKey).map_err(Crash)?;
        let prop_value = prop_name.as_str().and_then(|name| object.get(name));
        let Some(prop_value) = prop_value.filter(|v| truthy(v)) else {
            errors.push(Self::error(format!(
                "{} does not contain discriminating property {}",
                repr(instance),
                repr(prop_name)
            )));
            return Ok(());
        };
        // `discriminator.get("mapping", {}).get(prop_value)`
        let mapping = match discriminator.get("mapping") {
            None => None,
            Some(Value::Object(mapping)) => Some(mapping),
            Some(other) => {
                return Err(Crash(format!(
                    "'{}' object has no attribute 'get'",
                    python_type_name(other)
                )));
            }
        };
        pyobj::check_hashable(prop_value, pyobj::Hashed::DictKey).map_err(Crash)?;
        let mapped = mapping
            .and_then(|mapping| prop_value.as_str().and_then(|key| mapping.get(key)))
            .filter(|v| truthy(v));
        let reference = match mapped {
            Some(Value::String(reference)) => reference.clone(),
            Some(other) => {
                errors.push(Self::error(format!(
                    "{} mapped value for {} should be a string, was {}",
                    repr(instance),
                    repr(prop_value),
                    repr(other)
                )));
                return Ok(());
            }
            None => format!("#/components/schemas/{}", pyobj::py_str(prop_value)),
        };
        match self.docs.lookup_node(doc, &reference) {
            // `descend(instance, {"$ref": ref})`: one more `descend` and the `$ref` keyword.
            Ok(target) => {
                self.enter(2)?;
                self.descend(instance, target.value, target.doc, errors)?;
                self.frames -= 2;
                Ok(())
            }
            Err(error) if error.is_referencing() => {
                errors.push(Self::error(format!(
                    "{} reference {} could not be resolved",
                    repr(instance),
                    repr_str(&reference)
                )));
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }
}

/// Applies the `path` / `schema_path` arguments of one `descend` call to the errors it
/// produced, except to errors from a `false` schema met by that very call.
fn prefix(errors: &mut [ValueError], path: Option<PathItem>, schema_path: Option<PathItem>) {
    for error in errors {
        if error.bare_false {
            error.bare_false = false;
            continue;
        }
        if let Some(item) = &path {
            error.instance_path.push_front(item.clone());
        }
        if let Some(item) = &schema_path {
            error.schema_path.push_front(item.clone());
        }
    }
}

/// jsonschema's `multipleOf` test: whether `instance` (the number `x`) fails it.
/// A float divisor divides in floating point (`int(q) != q`), falling back to exact
/// fractions when the quotient overflows; otherwise `instance % divisor` decides.
pub(crate) fn multiple_of(x: Num, instance: &Value, divisor: &Value) -> Result<bool, Crash> {
    if let Value::Number(n) = divisor {
        if n.is_f64() {
            let d = n.as_f64().unwrap_or_default();
            if d == 0.0 {
                return Err(Crash(pyobj::zero_division("float division by zero")));
            }
            let quotient = x.as_f64() / d;
            if quotient.is_finite() {
                return Ok(quotient.trunc() != quotient);
            }
            return Ok(!exact_multiple(x, d));
        }
    }
    let Some(divisor) = pyobj::as_number(divisor) else {
        return Err(unsupported_operand("%", instance, divisor));
    };
    Ok(match (x, divisor) {
        (Num::Int(a), Num::Int(b)) => {
            if b == 0 {
                return Err(Crash(pyobj::zero_division("integer modulo by zero")));
            }
            a % b != 0
        }
        (Num::Float(a), Num::Int(b)) => {
            if b == 0 {
                return Err(Crash(pyobj::zero_division("float modulo by zero")));
            }
            #[allow(clippy::cast_precision_loss)]
            let b = b as f64;
            a % b != 0.0
        }
        // Integral divisors only reach here as ints or bools.
        (_, Num::Float(_)) => false,
    })
}

/// `(Fraction(x) / Fraction(d)).denominator == 1`, i.e. `d` divides `x` exactly.
fn exact_multiple(x: Num, d: f64) -> bool {
    /// `value` as `odd * 2**exponent` (`odd` is 0 for zero).
    fn split(mantissa: i128, exponent: i32) -> (i128, i32) {
        if mantissa == 0 {
            return (0, 0);
        }
        let zeros = mantissa.trailing_zeros();
        #[allow(clippy::cast_possible_wrap)]
        (mantissa >> zeros, exponent + zeros as i32)
    }
    fn decompose(f: f64) -> (i128, i32) {
        let bits = f.to_bits();
        let sign = if bits >> 63 == 0 { 1 } else { -1 };
        #[allow(clippy::cast_possible_truncation)]
        let exponent = ((bits >> 52) & 0x7ff) as i32;
        let fraction = i128::from(bits & ((1 << 52) - 1));
        let (mantissa, exponent) = if exponent == 0 {
            (fraction, -1074)
        } else {
            (fraction | (1 << 52), exponent - 1075)
        };
        split(sign * mantissa, exponent)
    }
    let (a, a_exp) = match x {
        Num::Int(i) => split(i, 0),
        Num::Float(f) => decompose(f),
    };
    let (b, b_exp) = decompose(d);
    a == 0 || (a % b == 0 && a_exp >= b_exp)
}

/// The exception `descend` raises for a schema that is neither a mapping nor a boolean:
/// `Resolver.in_subresource` reads its `id` (Draft 4 rules: `"$ref" in schema` first),
/// then `applicable_validators` calls `schema.items()`.
fn not_a_schema(schema: &Value) -> Crash {
    let has_ref = match schema {
        Value::String(s) => s.contains("$ref"),
        Value::Array(items) => items.iter().any(|item| item.as_str() == Some("$ref")),
        other => {
            return Crash(pyobj::not_a_container(other));
        }
    };
    let attribute = if has_ref { "items" } else { "get" };
    Crash(format!(
        "'{}' object has no attribute '{attribute}'",
        python_type_name(schema)
    ))
}

/// A number for jsonschema's `is_type(instance, "number")` (bools excluded).
pub(crate) fn number(value: &Value) -> Option<Num> {
    match value {
        Value::Number(n) => Some(pyobj::number(n)),
        _ => None,
    }
}

pub(crate) fn unsupported_comparison(op: &str, left: &Value, right: &Value) -> Crash {
    Crash(format!(
        "'{op}' not supported between instances of '{}' and '{}'",
        python_type_name(left),
        python_type_name(right)
    ))
}

fn unsupported_operand(op: &str, left: &Value, right: &Value) -> Crash {
    Crash(format!(
        "unsupported operand type(s) for {op}: '{}' and '{}'",
        python_type_name(left),
        python_type_name(right)
    ))
}
