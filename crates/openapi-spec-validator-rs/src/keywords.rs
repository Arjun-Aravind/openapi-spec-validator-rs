//! The semantic checks of upstream's `validation/keywords.py`, one function per
//! keyword validator, walking the spec in the same order with the same dedupe rules.
//!
//! Upstream implements these as generators; a failure surfaces either as a yielded
//! validation error or as an exception that aborts validation. Here [`Walker::emit`]
//! records errors and stops after the first one in "first error" mode, and [`Stop`]
//! carries the exception case.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde_json::Value;

use crate::document::{Crash, Documents, Key, Node};
use crate::error::{ErrorKind, ValidationError};
use crate::formats::FormatSet;
use crate::meta::{self, Dialect, OAS31_DIALECT_ID, OAS32_DIALECT_ID};
use crate::pyformat;
use crate::pyobj::{self, py_str, truthy};
use crate::pyrepr::{self, repr};
use crate::values::DefaultValidator;
use crate::version::SpecVersion;

/// Why the walk stopped early.
pub(crate) enum Stop {
    /// The first error was found in "first error" mode.
    Done,
    /// Upstream would raise a non-validation exception.
    Crash(String),
}

impl From<Crash> for Stop {
    fn from(Crash(message): Crash) -> Self {
        Self::Crash(message)
    }
}

pub(crate) type Step = Result<(), Stop>;

/// HTTP methods `PathValidator` treats as operations.
const OPERATIONS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

pub(crate) struct Walker<'d, 'a> {
    docs: &'d Documents<'a>,
    version: SpecVersion,
    first_only: bool,
    pub errors: Vec<ValidationError>,
    /// `SchemaValidator.visited_schema_ids`.
    visited: HashSet<usize>,
    /// `SchemaValidator.meta_checked_schema_ids`.
    meta_checked: HashSet<usize>,
    /// `OperationValidator.operation_ids_registry`.
    operation_ids: OperationIds<'d>,
    /// `OpenAPIV31SchemaValidator._default_jsonschema_dialect_id`.
    default_dialect: Option<String>,
    defaults: DefaultValidator<'d, 'a>,
    /// `_collect_properties` results by schema identity.
    collected: HashMap<usize, Vec<String>>,
    /// Python frames upstream's walk holds at this point: one per keyword validator call,
    /// from `SpecValidator.iter_errors` on (see `crate::frames`).
    frames: usize,
    /// Python's recursion limit.
    limit: usize,
    /// How deep the document nests: no schema in it nests deeper.
    nesting: usize,
}

impl<'d, 'a> Walker<'d, 'a> {
    pub fn new(
        docs: &'d Documents<'a>,
        version: SpecVersion,
        first_only: bool,
        stack: crate::context::PythonStack,
        nesting: usize,
    ) -> Self {
        Self {
            docs,
            version,
            first_only,
            errors: Vec::new(),
            visited: HashSet::new(),
            meta_checked: HashSet::new(),
            operation_ids: OperationIds::default(),
            default_dialect: None,
            defaults: DefaultValidator::new(docs, version),
            collected: HashMap::new(),
            frames: stack.spec_depth,
            limit: stack.limit,
            nesting,
        }
    }

    /// A call to another keyword validator: one more frame while it runs.
    fn nested<T>(&mut self, call: impl FnOnce(&mut Self) -> T) -> T {
        self.frames += 1;
        let result = call(self);
        self.frames -= 1;
        result
    }

    fn emit(&mut self, error: ValidationError) -> Step {
        self.errors.push(error);
        if self.first_only {
            Err(Stop::Done)
        } else {
            Ok(())
        }
    }

    fn emit_new(&mut self, kind: ErrorKind, message: String) -> Step {
        self.emit(ValidationError::new(kind, message))
    }

    fn resolve(&self, node: Node<'d>) -> Result<Node<'d>, Crash> {
        self.docs.resolve(node)
    }

    fn child(&self, node: Node<'d>, key: &str) -> Result<Node<'d>, Crash> {
        self.docs.child(node, key)
    }

    /// `(path / key).read_str()`.
    fn read_str(&self, node: Node<'d>, key: &str) -> Result<&'d str, Crash> {
        let value = self.child(node, key)?.value;
        value.as_str().ok_or_else(|| {
            Crash(format!(
                "Expected a string value, got {}",
                pyobj::type_class(value)
            ))
        })
    }

    /// `path[key]`: the resolved child value, `KeyError` if missing.
    fn item(&self, node: Node<'d>, key: &str) -> Result<&'d Value, Crash> {
        Ok(self.child(node, key)?.value)
    }

    /// `RootValidator`.
    pub fn root(&mut self, spec: Node<'d>) -> Step {
        self.nested(|walker| walker.root_validator(spec))
    }

    /// `RootValidator`.
    fn root_validator(&mut self, spec: Node<'d>) -> Step {
        let spec = self.resolve(spec)?;
        if contains(spec, "tags") {
            let tags = self.child(spec, "tags")?;
            match self.version {
                SpecVersion::V3_2 => self.nested(|walker| walker.tags_v32(tags))?,
                _ => self.nested(|walker| walker.tags(tags))?,
            }
        }
        if contains(spec, "paths") {
            let paths = self.child(spec, "paths")?;
            self.nested(|walker| walker.paths(paths))?;
        }
        if contains(spec, "components") {
            let components = self.child(spec, "components")?;
            self.nested(|walker| walker.components(components))?;
        }
        Ok(())
    }

    /// `TagsValidator`: tag names are unique.
    fn tags(&mut self, tags: Node<'d>) -> Step {
        let mut seen = HashSet::new();
        for tag in self.docs.children(tags, "tags")? {
            let tag = self.resolve(tag)?;
            let name = self.read_str(tag, "name")?;
            if seen.contains(name) {
                self.emit_new(
                    ErrorKind::OpenApiValidation,
                    format!("Duplicate tag name '{name}'"),
                )?;
            }
            seen.insert(name);
        }
        Ok(())
    }

    /// `OpenAPIV32TagsValidator`: unique names, known parents, no parent cycles.
    fn tags_v32(&mut self, tags: Node<'d>) -> Step {
        self.nested(|walker| walker.tags(tags))?;

        let mut seen = HashSet::new();
        // Insertion-ordered like the Python dict.
        let mut parents: Vec<(&'d str, Option<&'d str>)> = Vec::new();
        for tag in self.docs.children(tags, "tags")? {
            let tag = self.resolve(tag)?;
            let name = self.read_str(tag, "name")?;
            seen.insert(name);
            let parent = if contains(tag, "parent") {
                Some(self.read_str(tag, "parent")?)
            } else {
                None
            };
            match parents.iter_mut().find(|(n, _)| *n == name) {
                Some(entry) => entry.1 = parent,
                None => parents.push((name, parent)),
            }
        }
        let parent_of = |name: &str| {
            parents
                .iter()
                .find(|(n, _)| *n == name)
                .and_then(|(_, parent)| *parent)
        };

        for &(name, parent) in &parents {
            if let Some(parent) = parent {
                if !seen.contains(parent) {
                    self.emit_new(
                        ErrorKind::OpenApiValidation,
                        format!("Tag '{name}' references unknown parent tag '{parent}'"),
                    )?;
                }
            }
        }

        let mut reported = HashSet::new();
        for &(start, _) in &parents {
            let mut name = start;
            let mut trail: Vec<&str> = Vec::new();
            loop {
                if let Some(position) = trail.iter().position(|t| *t == name) {
                    let mut cycle = trail[position..].to_vec();
                    cycle.push(name);
                    let cycle = cycle.join(" -> ");
                    if reported.insert(cycle.clone()) {
                        self.emit_new(
                            ErrorKind::OpenApiValidation,
                            format!("Circular tag hierarchy detected: {cycle}"),
                        )?;
                    }
                    break;
                }
                trail.push(name);
                match parent_of(name) {
                    Some(parent) if seen.contains(parent) => name = parent,
                    _ => break,
                }
            }
        }
        Ok(())
    }

    /// `PathsValidator`.
    fn paths(&mut self, paths: Node<'d>) -> Step {
        for (url, path_item) in self.docs.entries(paths, "paths")? {
            let Key::Str(url) = url else {
                return Err(Stop::Crash(String::new()));
            };
            let path_item = self.resolve(path_item)?;
            self.nested(|walker| walker.path(url, path_item))?;
        }
        Ok(())
    }

    /// `PathValidator` (and `OpenAPIV32PathValidator` for 3.2).
    fn path(&mut self, url: &'d str, path_item: Node<'d>) -> Step {
        let mut parameters = None;
        if contains(path_item, "parameters") {
            let node = self.child(path_item, "parameters")?;
            self.nested(|walker| walker.parameters(node))?;
            parameters = Some(node);
        }
        let v32 = self.version == SpecVersion::V3_2;
        for (field, operation) in self.docs.entries(path_item, url)? {
            let Key::Str(field) = field else {
                return Err(Stop::Crash(String::new()));
            };
            if OPERATIONS.contains(&field) || (v32 && field == "query") {
                let operation = self.resolve(operation)?;
                self.nested(|walker| walker.operation(url, field, operation, parameters))?;
            } else if v32 && field == "additionalOperations" {
                let operations = self.resolve(operation)?;
                for (name, operation) in self.docs.entries(operations, field)? {
                    let Key::Str(name) = name else {
                        return Err(Stop::Crash(String::new()));
                    };
                    let operation = self.resolve(operation)?;
                    self.nested(|walker| walker.operation(url, name, operation, parameters))?;
                }
            }
        }
        Ok(())
    }

    /// `OperationValidator`: unique operationIds, responses, parameters, and path
    /// parameters matching the URL template.
    fn operation(
        &mut self,
        url: &str,
        name: &str,
        operation: Node<'d>,
        path_parameters: Option<Node<'d>>,
    ) -> Step {
        if contains(operation, "operationId") {
            let operation_id = self.item(operation, "operationId")?;
            if !operation_id.is_null() && self.operation_ids.contains(operation_id) {
                self.emit_new(
                    ErrorKind::DuplicateOperationId,
                    format!(
                        "Operation ID '{}' for '{name}' in '{url}' is not unique",
                        py_str(operation_id)
                    ),
                )?;
            }
            self.operation_ids.push(operation_id);
        }

        if contains(operation, "responses") {
            let responses = self.child(operation, "responses")?;
            self.nested(|walker| walker.responses(responses))?;
        }

        let mut names = Vec::new();
        if contains(operation, "parameters") {
            let parameters = self.child(operation, "parameters")?;
            self.nested(|walker| walker.parameters(parameters))?;
            names.extend(self.path_param_names(parameters)?);
        }
        if let Some(parameters) = path_parameters {
            names.extend(self.path_param_names(parameters)?);
        }

        let all_params: BTreeSet<&str> = names.into_iter().collect();
        let url_params: BTreeSet<String> = pyformat::formatter_field_names(url)
            .map_err(Stop::Crash)?
            .into_iter()
            .flatten()
            .filter(|field| !field.is_empty())
            .collect();

        for param in &url_params {
            if !all_params.contains(param.as_str()) {
                self.emit_unresolved_param(param, name, url)?;
            }
        }
        for param in &all_params {
            if !url_params.contains(*param) {
                self.emit_unresolved_param(param, name, url)?;
            }
        }
        Ok(())
    }

    fn emit_unresolved_param(&mut self, param: &str, name: &str, url: &str) -> Step {
        self.emit_new(
            ErrorKind::UnresolvableParameter,
            format!("Path parameter '{param}' for '{name}' operation in '{url}' was not resolved"),
        )
    }

    /// `OperationValidator._get_path_param_names`.
    fn path_param_names(&self, parameters: Node<'d>) -> Result<Vec<&'d str>, Crash> {
        let mut names = Vec::new();
        for param in self.docs.children(parameters, "parameters")? {
            let param = self.resolve(param)?;
            if self.read_str(param, "in")? == "path" {
                names.push(self.read_str(param, "name")?);
            }
        }
        Ok(names)
    }

    /// `ParametersValidator`: each parameter, and no duplicate `(name, in)` pairs.
    fn parameters(&mut self, parameters: Node<'d>) -> Step {
        let mut seen: Vec<(&'d Value, &'d Value)> = Vec::new();
        for parameter in self.docs.children(parameters, "parameters")? {
            let parameter = self.resolve(parameter)?;
            self.nested(|walker| walker.parameter(parameter))?;

            let name = self.item(parameter, "name")?;
            let location = self.item(parameter, "in")?;
            // jsonschema-path hands out lists and mappings as `SchemaPath`s, which are
            // hashable and equal only to the same path: such a key is never a duplicate.
            let container = |v: &Value| v.is_array() || pyobj::as_dict(v).is_some();
            if container(name) || container(location) {
                continue;
            }
            let duplicate = seen
                .iter()
                .any(|(n, l)| pyobj::py_eq(n, name) && pyobj::py_eq(l, location));
            if duplicate {
                self.emit_new(
                    ErrorKind::ParameterDuplicate,
                    format!("Duplicate parameter '{}'", py_str(name)),
                )?;
            }
            seen.push((name, location));
        }
        Ok(())
    }

    /// `ParameterValidator`, plus `OpenAPIV2ParameterValidator`'s `default` check.
    fn parameter(&mut self, parameter: Node<'d>) -> Step {
        if self.version == SpecVersion::V2_0 {
            // `OpenAPIV2ParameterValidator.__call__` runs the base validator's.
            return self.nested(|walker| walker.parameter_validator(parameter));
        }
        self.parameter_validator(parameter)
    }

    /// `ParameterValidator`.
    fn parameter_validator(&mut self, parameter: Node<'d>) -> Step {
        if contains(parameter, "schema") {
            let schema = self.child(parameter, "schema")?;
            self.nested(|walker| walker.schema(schema, true, false))?;
        }
        if self.version == SpecVersion::V2_0 && contains(parameter, "default") {
            let default = self.item(parameter, "default")?;
            self.default_value(parameter, default)?;
        }
        Ok(())
    }

    /// `ResponsesValidator`.
    fn responses(&mut self, responses: Node<'d>) -> Step {
        for (code, response) in self.docs.entries(responses, "responses")? {
            if !matches!(code, Key::Str(_)) {
                return Err(Stop::Crash(String::new()));
            }
            let response = self.resolve(response)?;
            self.nested(|walker| walker.response(response))?;
        }
        Ok(())
    }

    /// `OpenAPIV2ResponseValidator` / `OpenAPIV3ResponseValidator`.
    fn response(&mut self, response: Node<'d>) -> Step {
        if self.version == SpecVersion::V2_0 {
            if contains(response, "schema") {
                let schema = self.child(response, "schema")?;
                self.nested(|walker| walker.schema(schema, true, false))?;
            }
        } else if contains(response, "content") {
            let content = self.child(response, "content")?;
            self.nested(|walker| walker.content(content))?;
        }
        Ok(())
    }

    /// `ContentValidator`.
    fn content(&mut self, content: Node<'d>) -> Step {
        for (mimetype, media_type) in self.docs.entries(content, "content")? {
            if !matches!(mimetype, Key::Str(_)) {
                return Err(Stop::Crash(String::new()));
            }
            let media_type = self.resolve(media_type)?;
            self.nested(|walker| walker.media_type(media_type))?;
        }
        Ok(())
    }

    /// `MediaTypeValidator`.
    fn media_type(&mut self, media_type: Node<'d>) -> Step {
        if contains(media_type, "schema") {
            let schema = self.child(media_type, "schema")?;
            self.nested(|walker| walker.schema(schema, true, false))?;
        }
        Ok(())
    }

    /// `ComponentsValidator`: only `components.schemas` is checked upstream.
    fn components(&mut self, components: Node<'d>) -> Step {
        if contains(components, "schemas") {
            let schemas = self.child(components, "schemas")?;
            self.nested(|walker| walker.schemas(schemas))?;
        }
        Ok(())
    }

    /// `SchemasValidator`.
    fn schemas(&mut self, schemas: Node<'d>) -> Step {
        for (_, schema) in self.docs.entries(schemas, "schemas")? {
            let schema = self.resolve(schema)?;
            self.nested(|walker| walker.schema(schema, true, false))?;
        }
        Ok(())
    }

    /// `SchemaValidator.__call__`: meta-checks top-level schemas, recurses into
    /// subschemas, checks `required` against defined properties (only when `allOf` is
    /// present) and validates `default` values.
    ///
    /// Visits are deduplicated by identity, first visit wins: a schema first reached
    /// as a nested subschema never gets the `required` check.
    fn schema(&mut self, schema: Node<'d>, require_properties: bool, meta_checked: bool) -> Step {
        crate::context::check();
        let value = schema.value;
        if pyobj::as_dict(value).is_none() && !value.is_boolean() {
            return self.emit_new(
                ErrorKind::OpenApiValidation,
                format!("{} is not of type 'object', 'boolean'", repr(value)),
            );
        }

        let id = schema.id();
        if !meta_checked && self.meta_checked.insert(id) {
            if let Some(error) = self.validate_schema_meta(schema)? {
                return self.emit(error);
            }
        }

        if !self.visited.insert(id) {
            return Ok(());
        }

        let mut nested_properties: Vec<String> = Vec::new();
        if contains(schema, "allOf") {
            let all_of = self.child(schema, "allOf")?;
            for inner in self.docs.children(all_of, "allOf")? {
                let inner = self.resolve(inner)?;
                self.nested(|walker| walker.schema(inner, false, true))?;
                nested_properties.extend(self.collect_properties(inner, &mut Vec::new())?);
            }
        }
        for keyword in ["anyOf", "oneOf"] {
            if contains(schema, keyword) {
                let list = self.child(schema, keyword)?;
                for inner in self.docs.children(list, keyword)? {
                    let inner = self.resolve(inner)?;
                    self.nested(|walker| walker.schema(inner, false, true))?;
                }
            }
        }
        for keyword in ["not", "items"] {
            if contains(schema, keyword) {
                let inner = self.child(schema, keyword)?;
                self.nested(|walker| walker.schema(inner, false, true))?;
            }
        }
        if contains(schema, "properties") {
            let properties = self.child(schema, "properties")?;
            for (_, property) in self.docs.entries(properties, "properties")? {
                let property = self.resolve(property)?;
                self.nested(|walker| walker.schema(property, false, true))?;
            }
        }

        let required = if contains(schema, "required") {
            self.item(schema, "required")?
        } else {
            &Value::Null
        };
        let properties: Vec<&str> = if contains(schema, "properties") {
            let node = self.child(schema, "properties")?;
            match node.value {
                Value::Object(map) => map.keys().map(String::as_str).collect(),
                Value::Array(_) => Vec::new(),
                _ => return Err(Crash::key_error("properties").into()),
            }
        } else {
            Vec::new()
        };
        let extra_properties = if contains(schema, "allOf") {
            undefined_required(required, &properties, &nested_properties)?
        } else {
            Vec::new()
        };
        if !extra_properties.is_empty() && require_properties {
            let listed = Value::Array(extra_properties);
            self.emit_new(
                ErrorKind::ExtraParameters,
                format!(
                    "Required list has not defined properties: {}",
                    repr(&listed)
                ),
            )?;
        }

        if contains(schema, "default") {
            let default = self.item(schema, "default")?;
            let nullable = if contains(schema, "nullable") {
                self.item(schema, "nullable")?
            } else {
                &Value::Bool(false)
            };
            if !default.is_null() || nullable != &Value::Bool(true) {
                self.default_value(schema, default)?;
            }
        }
        Ok(())
    }

    /// `SchemaValidator._collect_properties`: property names reachable through
    /// `allOf`/`anyOf`/`oneOf`/`items`/`not`. A `$ref` cycle along those keywords makes
    /// upstream recurse until Python's recursion limit.
    fn collect_properties(
        &mut self,
        schema: Node<'d>,
        stack: &mut Vec<usize>,
    ) -> Result<Vec<String>, Stop> {
        let id = schema.id();
        if let Some(cached) = self.collected.get(&id) {
            return Ok(cached.clone());
        }
        if stack.contains(&id) {
            return Err(Crash::recursion().into());
        }
        stack.push(id);

        let mut props = Vec::new();
        if contains(schema, "properties") {
            let properties = self.child(schema, "properties")?;
            match properties.value {
                Value::Object(map) => props.extend(map.keys().cloned()),
                Value::Array(_) => {}
                _ => return Err(Crash::key_error("properties").into()),
            }
        }
        for keyword in ["allOf", "anyOf", "oneOf"] {
            if contains(schema, keyword) {
                let list = self.child(schema, keyword)?;
                for sub in self.docs.children(list, keyword)? {
                    let sub = self.resolve(sub)?;
                    props.extend(self.collect_properties(sub, stack)?);
                }
            }
        }
        for keyword in ["items", "not"] {
            if contains(schema, keyword) {
                let sub = self.child(schema, keyword)?;
                props.extend(self.collect_properties(sub, stack)?);
            }
        }

        stack.pop();
        self.collected.insert(id, props.clone());
        Ok(props)
    }

    /// `SchemaValidator._validate_schema_meta`: the schema itself must be valid for its
    /// JSON Schema dialect.
    fn validate_schema_meta(&mut self, schema: Node<'d>) -> Result<Option<ValidationError>, Stop> {
        let (dialect, formats) = match self.version {
            SpecVersion::V2_0 | SpecVersion::V3_0 => (Dialect::Draft4, FormatSet::Oas30),
            SpecVersion::V3_1 | SpecVersion::V3_2 => {
                let formats = if self.version == SpecVersion::V3_1 {
                    FormatSet::Oas31
                } else {
                    FormatSet::Oas32
                };
                let dialect_id = match schema.value.get("$schema") {
                    Some(Value::String(id)) => id.clone(),
                    Some(other) => {
                        return Ok(Some(ValidationError::new(
                            ErrorKind::OpenApiValidation,
                            format!("Unknown JSON Schema dialect: {}", repr(other)),
                        )));
                    }
                    None => self.default_dialect()?,
                };
                match Dialect::from_uri(&dialect_id) {
                    Some(dialect) => (dialect, formats),
                    None => {
                        return Ok(Some(ValidationError::new(
                            ErrorKind::OpenApiValidation,
                            format!(
                                "Unknown JSON Schema dialect: {}",
                                pyrepr::repr_str(&dialect_id)
                            ),
                        )));
                    }
                }
            }
        };
        let validator = meta::meta_check_validator(dialect, formats);
        // `_validate_schema_meta`, `check_openapi_schema`, then the check's `iter_errors`.
        let budget = crate::frames::Budget {
            root: self.frames + 3,
            limit: self.limit,
        };
        let schemas = meta::meta_check_schemas(dialect);
        // A schema from this document nests no deeper than the document.
        let nesting = (schema.doc == 0).then_some(self.nesting);
        if let Some(probe) = crate::frames::find(schema.value, &schemas, budget, true, nesting) {
            return self.recursion_in_check(schema.value, validator, &schemas, &probe);
        }
        if validator.is_valid(schema.value) {
            return Ok(None);
        }
        // `check_schema` raises the first error Python's iteration finds.
        let error = meta::first_error(
            validator.iter_errors(schema.value),
            &meta::meta_check_schemas(dialect),
            schema.value,
            |_| false,
        );
        if let Some(message) = error.as_ref().and_then(meta::crash_message) {
            return Err(Stop::Crash(message.to_owned()));
        }
        Ok(error)
    }

    /// A dialect check that exceeds the recursion limit at `probe`'s crossing: the first
    /// error, if Python's lazy check yields it before getting that deep, else
    /// `RecursionError`.
    fn recursion_in_check(
        &self,
        schema: &Value,
        validator: &jsonschema::Validator,
        schemas: &meta::Schemas<'_>,
        probe: &crate::frames::Probe<'_>,
    ) -> Result<Option<ValidationError>, Stop> {
        let recursion = || Stop::Crash(Crash::recursion().0);
        let Some(crossing) = probe.crossing.as_ref() else {
            return Err(recursion());
        };
        let pruned = crate::frames::pruned(schema, &probe.prune);
        let first = meta::first_error(validator.iter_errors(&pruned), schemas, &pruned, |error| {
            error.schema_error.as_deref().is_some_and(|details| {
                probe.prune.iter().any(|location| {
                    details.instance_path.len() > location.len()
                        && details.instance_path.starts_with(location)
                })
            })
        });
        match first {
            Some(error) if crate::frames::precedes(schemas, schema, &error, crossing) => {
                if let Some(message) = meta::crash_message(&error) {
                    return Err(Stop::Crash(message.to_owned()));
                }
                Ok(Some(error))
            }
            _ => Err(recursion()),
        }
    }

    /// The spec's `jsonSchemaDialect`, or the version's default dialect.
    fn default_dialect(&mut self) -> Result<String, Crash> {
        if let Some(dialect) = &self.default_dialect {
            return Ok(dialect.clone());
        }
        let root = self.resolve(self.docs.root())?;
        let dialect = if contains(root, "jsonSchemaDialect") {
            self.read_str(root, "jsonSchemaDialect")?.to_owned()
        } else if self.version == SpecVersion::V3_2 {
            OAS32_DIALECT_ID.to_owned()
        } else {
            OAS31_DIALECT_ID.to_owned()
        };
        self.default_dialect = Some(dialect.clone());
        Ok(dialect)
    }

    /// `ValueValidator`: `default` must validate against its own schema.
    fn default_value(&mut self, schema: Node<'d>, value: &'d Value) -> Step {
        let errors = self.defaults.validate(schema, value, self.first_only)?;
        for error in errors {
            self.emit(error)?;
        }
        Ok(())
    }
}

/// `"key" in path` on a resolved node.
fn contains(node: Node<'_>, key: &str) -> bool {
    pyobj::as_dict(node.value).is_some_and(|map| map.contains_key(key))
}

/// `list(set(required) - set(properties) - set(nested_properties))`.
///
/// Upstream's order is Python set iteration order, which varies between runs for
/// strings; this keeps the order of `required`.
fn undefined_required(
    required: &Value,
    properties: &[&str],
    nested: &[String],
) -> Result<Vec<Value>, Stop> {
    if !truthy(required) {
        return Ok(Vec::new());
    }
    let items = pyobj::iterate(required).map_err(Stop::Crash)?;
    let mut extra: Vec<Value> = Vec::new();
    for item in items {
        pyobj::check_hashable(&item, pyobj::Hashed::SetElement).map_err(Stop::Crash)?;
        let defined = item
            .as_str()
            .is_some_and(|name| properties.contains(&name) || nested.iter().any(|n| n == name));
        if !defined && !extra.iter().any(|e| pyobj::py_eq(e, &item)) {
            extra.push(item);
        }
    }
    Ok(extra)
}

/// Python list membership (`in`) over operationIds, with a fast path for strings.
#[derive(Default)]
struct OperationIds<'d> {
    strings: HashSet<&'d str>,
    others: Vec<&'d Value>,
}

impl<'d> OperationIds<'d> {
    fn contains(&self, id: &Value) -> bool {
        match id {
            Value::String(s) => self.strings.contains(s.as_str()),
            other => self.others.iter().any(|seen| pyobj::py_eq(seen, other)),
        }
    }

    fn push(&mut self, id: &'d Value) {
        match id {
            Value::String(s) => {
                self.strings.insert(s);
            }
            other => self.others.push(other),
        }
    }
}
