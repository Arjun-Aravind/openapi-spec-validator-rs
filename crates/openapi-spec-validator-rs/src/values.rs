//! Validation of `default` values against their schema (upstream's `ValueValidator`).
//!
//! Swagger 2.0 and OpenAPI 3.0 use openapi-schema-validator's `OAS30Validator`, a
//! Draft 4 variant with OpenAPI keywords; it is ported here keyword by keyword so
//! that iteration order, messages and edge-case exceptions match. OpenAPI 3.1 and 3.2
//! use `OAS31Validator` / `OAS32Validator` (Draft 2020-12 with OpenAPI formats), which
//! is delegated to the `jsonschema` crate with Python-compatible `format`/`pattern`.

use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};

use jsonschema::paths::Location;
use jsonschema::{Keyword, Registry};
use serde_json::{Map, Value};

use crate::document::{Crash, Documents, Node};
use crate::error::{SchemaErrorDetails, ValidationError};
use crate::formats::{self, FormatOutcome, FormatSet};
use crate::meta;
use crate::pyobj::{self, Num, json_equal, num_cmp, truthy};
use crate::pyregex::{self, PyRegex};
use crate::pyrepr::{PathItem, repr, repr_float, repr_str};
use crate::schema_path::Resource;
use crate::version::{SpecVersion, python_type_name};

/// Upstream recurses in Python; a reference cycle hits the recursion limit.
const MAX_DEPTH: usize = 250;

pub(crate) struct DefaultValidator<'d, 'a> {
    docs: &'d Documents<'a>,
    version: SpecVersion,
    regexes: HashMap<String, Result<PyRegex, String>>,
    /// Pointer of every container node, built on first use (3.1+ only).
    pointers: Option<HashMap<usize, (usize, String)>>,
    registry: Option<Registry<'d>>,
}

impl<'d, 'a> DefaultValidator<'d, 'a> {
    pub fn new(docs: &'d Documents<'a>, version: SpecVersion) -> Self {
        Self {
            docs,
            version,
            regexes: HashMap::new(),
            pointers: None,
            registry: None,
        }
    }

    /// All errors of `value` against `schema`, or the exception upstream would raise.
    pub fn validate(
        &mut self,
        schema: Node<'d>,
        value: &'d Value,
    ) -> Result<Vec<ValidationError>, Crash> {
        match self.version {
            SpecVersion::V2_0 | SpecVersion::V3_0 => {
                let mut errors = Vec::new();
                let mut eval = Oas30 {
                    docs: self.docs,
                    regexes: &mut self.regexes,
                    depth: 0,
                };
                eval.descend(value, schema.value, schema.doc, &mut errors)?;
                Ok(errors
                    .into_iter()
                    .map(Oas30Error::into_validation_error)
                    .collect())
            }
            SpecVersion::V3_1 | SpecVersion::V3_2 => self.validate_2020(schema, value),
        }
    }

    fn validate_2020(
        &mut self,
        schema: Node<'d>,
        value: &'d Value,
    ) -> Result<Vec<ValidationError>, Crash> {
        let formats = if self.version == SpecVersion::V3_1 {
            FormatSet::Oas31
        } else {
            FormatSet::Oas32
        };
        let docs = self.docs;
        let pointers = self.pointers.get_or_insert_with(|| index_pointers(docs));
        let reference = match pointers.get(&schema.id()) {
            Some((doc, pointer)) => {
                let uri = document_uri(docs, *doc);
                serde_json::json!({ "$ref": format!("{uri}#{pointer}") })
            }
            // Boolean schemas have no address worth indexing; validate them inline.
            None => schema.value.clone(),
        };
        let registry = match &self.registry {
            Some(registry) => registry,
            None => {
                let mut builder = Registry::new();
                for doc in 0..docs_count(docs) {
                    if let Some(root) = document_value(docs, doc) {
                        builder = builder
                            .add(document_uri(docs, doc), root)
                            .map_err(|e| Crash(e.to_string()))?;
                    }
                }
                self.registry = Some(builder.prepare().map_err(|e| Crash(e.to_string()))?);
                self.registry.as_ref().expect("registry was just built")
            }
        };
        let validator = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .with_registry(registry)
            .with_base_uri("urn:openapi-spec-validator-rs:default")
            .should_validate_formats(true)
            .with_keyword("format", meta::python_format(formats))
            .with_keyword("pattern", py_pattern)
            .build(&reference)
            .map_err(|e| Crash(e.to_string()))?;
        let uris: Vec<String> = (0..docs_count(docs))
            .map(|doc| document_uri(docs, doc))
            .collect();
        let mut resources = vec![Resource {
            uri: "urn:openapi-spec-validator-rs:default",
            contents: &reference,
        }];
        resources.extend((0..docs_count(docs)).filter_map(|doc| {
            Some(Resource {
                uri: &uris[doc],
                contents: document_value(docs, doc)?,
            })
        }));
        let schemas = meta::Schemas {
            resources,
            ref_overrides: false,
        };
        let errors = meta::convert_errors(validator.iter_errors(value), &schemas, value);
        if let Some(message) = errors.iter().find_map(meta::crash_message) {
            return Err(Crash(message.to_owned()));
        }
        Ok(errors)
    }
}

/// Synthetic URI of document `doc` for the default-value registry.
fn document_uri(docs: &Documents<'_>, doc: usize) -> String {
    let uri = docs.uri(doc);
    if uri.is_empty() {
        format!("urn:openapi-spec-validator-rs:doc:{doc}")
    } else {
        uri.to_owned()
    }
}

fn docs_count(docs: &Documents<'_>) -> usize {
    (0..).take_while(|i| docs.has_doc(*i)).count()
}

fn document_value<'d>(docs: &'d Documents<'_>, doc: usize) -> Option<&'d Value> {
    docs.doc_value(doc)
}

/// Maps the address of every container value to its document and JSON pointer.
fn index_pointers(docs: &Documents<'_>) -> HashMap<usize, (usize, String)> {
    fn walk(
        value: &Value,
        doc: usize,
        pointer: &mut String,
        out: &mut HashMap<usize, (usize, String)>,
    ) {
        if !(value.is_object() || value.is_array()) {
            return;
        }
        out.entry(std::ptr::from_ref(value) as usize)
            .or_insert_with(|| (doc, pointer.clone()));
        let len = pointer.len();
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    pointer.push('/');
                    pointer.push_str(&key.replace('~', "~0").replace('/', "~1"));
                    walk(child, doc, pointer, out);
                    pointer.truncate(len);
                }
            }
            Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    pointer.push('/');
                    pointer.push_str(&i.to_string());
                    walk(child, doc, pointer, out);
                    pointer.truncate(len);
                }
            }
            _ => {}
        }
    }
    let mut out = HashMap::new();
    for doc in 0..docs_count(docs) {
        if let Some(root) = document_value(docs, doc) {
            walk(root, doc, &mut String::new(), &mut out);
        }
    }
    // Pointers are used inside URI fragments.
    for (_, pointer) in out.values_mut() {
        *pointer = percent_encoding::utf8_percent_encode(pointer, FRAGMENT).to_string();
    }
    out
}

const FRAGMENT: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'<')
    .add(b'>')
    .add(b'`')
    .add(b'#')
    .add(b'%')
    .add(b'{')
    .add(b'}')
    .add(b'|')
    .add(b'\\')
    .add(b'^')
    .add(b'[')
    .add(b']');

/// `pattern` with Python `re.search` semantics; a pattern `re` rejects raises upstream.
struct PyPattern {
    pattern: String,
    regex: Result<PyRegex, String>,
}

fn py_pattern<'a>(
    _: &'a Map<String, Value>,
    value: &'a Value,
    _: Location,
) -> Result<Box<dyn for<'i> Keyword<'i>>, jsonschema::ValidationError<'a>> {
    let pattern = value.as_str().unwrap_or_default().to_owned();
    let regex = PyRegex::compile(&pattern).map_err(|error| error.python_str(&pattern));
    Ok(Box::new(PyPattern { pattern, regex }))
}

impl<'i> Keyword<'i> for PyPattern {
    fn validate(&self, instance: &'i Value) -> Result<(), jsonschema::ValidationError<'i>> {
        let Value::String(text) = instance else {
            return Ok(());
        };
        let regex = self
            .regex
            .as_ref()
            .map_err(|message| meta::crash_error(message))?;
        match regex.search(text) {
            Ok(true) => Ok(()),
            Ok(false) => Err(jsonschema::ValidationError::custom(format!(
                "{} does not match {}",
                repr(instance),
                repr_str(&self.pattern)
            ))),
            Err(message) => Err(meta::crash_error(&message)),
        }
    }

    fn is_valid(&self, instance: &'i Value) -> bool {
        self.validate(instance).is_ok()
    }
}

/// An error produced by the OAS 3.0 evaluator, with jsonschema's relative paths.
struct Oas30Error {
    message: String,
    /// `None` while unset; jsonschema fills it in at the keyword that reported the error.
    /// Errors from a `false` schema keep `Some("")`, i.e. `validator=None`.
    keyword: Option<String>,
    /// Set for an error from a `false` schema until it leaves the `descend` that met that
    /// schema: Python's `descend` returns before adding its path segments.
    bare_false: bool,
    schema_path: VecDeque<PathItem>,
    instance_path: VecDeque<PathItem>,
    schema: Value,
    instance: Value,
    cause: Option<String>,
    context: Vec<Oas30Error>,
}

impl Oas30Error {
    fn into_validation_error(self) -> ValidationError {
        let context = self
            .context
            .into_iter()
            .map(Oas30Error::into_validation_error)
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
    depth: usize,
}

impl<'d> Oas30<'d, '_, '_> {
    /// `Validator.descend`/`iter_errors`: applies every known keyword of `schema`, in
    /// schema order (OAS30Validator applies `$ref` siblings too).
    fn descend(
        &mut self,
        instance: &'d Value,
        schema: &'d Value,
        doc: usize,
        errors: &mut Vec<Oas30Error>,
    ) -> Result<(), Crash> {
        let map = match schema {
            Value::Bool(true) => return Ok(()),
            Value::Bool(false) => {
                errors.push(Oas30Error {
                    message: format!("False schema does not allow {}", repr(instance)),
                    keyword: Some(String::new()),
                    bare_false: true,
                    schema_path: VecDeque::new(),
                    instance_path: VecDeque::new(),
                    schema: schema.clone(),
                    instance: instance.clone(),
                    cause: None,
                    context: Vec::new(),
                });
                return Ok(());
            }
            Value::Object(map) => map,
            other => {
                return Err(Crash(format!(
                    "'{}' object has no attribute 'items'",
                    python_type_name(other)
                )));
            }
        };
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(Crash::recursion());
        }
        for (keyword, value) in map {
            let start = errors.len();
            self.keyword(keyword, value, instance, map, doc, errors)?;
            for error in &mut errors[start..] {
                if error.keyword.is_none() {
                    error.keyword = Some(keyword.clone());
                    error.schema = schema.clone();
                    error.instance = instance.clone();
                }
                if keyword != "$ref" {
                    error.schema_path.push_front(PathItem::Key(keyword.clone()));
                }
            }
        }
        self.depth -= 1;
        Ok(())
    }

    fn is_valid(
        &mut self,
        instance: &'d Value,
        schema: &'d Value,
        doc: usize,
    ) -> Result<bool, Crash> {
        let mut errors = Vec::new();
        self.descend(instance, schema, doc, &mut errors)?;
        Ok(errors.is_empty())
    }

    fn error(message: String) -> Oas30Error {
        Oas30Error {
            message,
            keyword: None,
            bare_false: false,
            schema_path: VecDeque::new(),
            instance_path: VecDeque::new(),
            schema: Value::Null,
            instance: Value::Null,
            cause: None,
            context: Vec::new(),
        }
    }

    /// `TypeChecker.is_type` of the OAS 3.0 type checker.
    fn is_type(instance: &Value, name: &str, schema: &Map<String, Value>) -> Result<bool, Crash> {
        meta::is_draft4_type(instance, name).ok_or_else(|| {
            Crash(format!(
                "Unknown type {} for validator with schema:\n    {}\n\nWhile checking instance:\n    {}",
                repr_str(name),
                repr(&Value::Object(schema.clone())),
                repr(instance)
            ))
        })
    }

    #[allow(clippy::too_many_lines)]
    fn keyword(
        &mut self,
        keyword: &str,
        value: &'d Value,
        instance: &'d Value,
        schema: &'d Map<String, Value>,
        doc: usize,
        errors: &mut Vec<Oas30Error>,
    ) -> Result<(), Crash> {
        let inst = || repr(instance);
        match keyword {
            "multipleOf" => {
                let Some(x) = number(instance) else {
                    return Ok(());
                };
                let Some(divisor) = pyobj::as_number(value) else {
                    return Err(unsupported_operand("%", instance, value));
                };
                let failed = match (x, divisor) {
                    (_, Num::Float(d)) => {
                        if d == 0.0 {
                            return Err(Crash("float division by zero".to_owned()));
                        }
                        let quotient = x.as_f64() / d;
                        if quotient.is_nan() {
                            return Err(Crash("cannot convert float NaN to integer".to_owned()));
                        }
                        quotient.is_finite() && quotient.trunc() != quotient
                    }
                    (Num::Int(a), Num::Int(b)) => {
                        if b == 0 {
                            return Err(Crash("integer modulo by zero".to_owned()));
                        }
                        a % b != 0
                    }
                    (Num::Float(a), Num::Int(b)) => {
                        if b == 0 {
                            return Err(Crash("float modulo by zero".to_owned()));
                        }
                        #[allow(clippy::cast_precision_loss)]
                        let b = b as f64;
                        a % b != 0.0
                    }
                };
                if failed {
                    let divisor = match divisor {
                        Num::Float(f) => repr_float(f),
                        Num::Int(_) => repr(value),
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
                let Value::Object(map) = instance else {
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
                    return Err(Crash(format!(
                        "unhashable type: '{}'",
                        python_type_name(value)
                    )));
                };
                if instance.is_null() {
                    if schema.get("nullable") == Some(&Value::Bool(true)) {
                        return Ok(());
                    }
                    errors.push(Self::error("None for not nullable".to_owned()));
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
                            }
                        }
                        "anyOf" => {
                            let mut all = Vec::new();
                            let mut matched = false;
                            for (index, subschema) in subschemas.iter().enumerate() {
                                let mut errs = Vec::new();
                                self.descend(instance, subschema, doc, &mut errs)?;
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
                                let mut errs = Vec::new();
                                self.descend(instance, subschema, doc, &mut errs)?;
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
                }
            }
            "properties" => {
                let Value::Object(object) = instance else {
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
                    }
                }
            }
            "required" => {
                let Value::Object(object) = instance else {
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
                }
            }
            "additionalProperties" => {
                let Value::Object(object) = instance else {
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
                let Value::String(reference) = value else {
                    return Err(Crash(format!(
                        "unhashable type: '{}'",
                        python_type_name(value)
                    )));
                };
                let target = self.docs.lookup_node(doc, reference)?;
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
        errors: &mut Vec<Oas30Error>,
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
        errors: &mut Vec<Oas30Error>,
    ) -> Result<(), Crash> {
        let discriminator = &schema["discriminator"];
        let Value::Object(discriminator) = discriminator else {
            return Err(Crash(format!(
                "'{}' object is not subscriptable",
                python_type_name(discriminator)
            )));
        };
        let prop_name = discriminator
            .get("propertyName")
            .ok_or_else(|| Crash::key_error("propertyName"))?;
        let Value::Object(object) = instance else {
            errors.push(Self::error(format!(
                "{} is not of type 'object'",
                repr(instance)
            )));
            return Ok(());
        };
        let prop_value = prop_name.as_str().and_then(|name| object.get(name));
        let Some(prop_value) = prop_value.filter(|v| truthy(v)) else {
            errors.push(Self::error(format!(
                "{} does not contain discriminating property {}",
                repr(instance),
                repr(prop_name)
            )));
            return Ok(());
        };
        let mapped = discriminator
            .get("mapping")
            .and_then(Value::as_object)
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
            Ok(target) => self.descend(instance, target.value, target.doc, errors),
            Err(_) => {
                errors.push(Self::error(format!(
                    "{} reference {} could not be resolved",
                    repr(instance),
                    repr_str(&reference)
                )));
                Ok(())
            }
        }
    }
}

/// Applies the `path` / `schema_path` arguments of one `descend` call to the errors it
/// produced, except to errors from a `false` schema met by that very call.
fn prefix(errors: &mut [Oas30Error], path: Option<PathItem>, schema_path: Option<PathItem>) {
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

/// A number for jsonschema's `is_type(instance, "number")` (bools excluded).
fn number(value: &Value) -> Option<Num> {
    match value {
        Value::Number(n) => Some(pyobj::number(n)),
        _ => None,
    }
}

fn unsupported_comparison(op: &str, left: &Value, right: &Value) -> Crash {
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
