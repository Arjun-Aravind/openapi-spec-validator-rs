//! Documents holding Python objects of types JSON has no counterpart for (`datetime.date`
//! from `yaml.safe_load`, `bytes`, `set`), as the Python bindings pass them in: jsonschema
//! treats them as objects of no type and prints their `repr()`. Expected results were
//! recorded by running openapi-spec-validator 0.9.0 on the same Python objects.

use openapi_spec_validator_rs::pyrepr::foreign_value;
use openapi_spec_validator_rs::{ErrorMode, Options, Outcome, validate};
use serde_json::{Value, json};

fn date() -> Value {
    foreign_value(
        "datetime.date",
        "<class 'datetime.date'>",
        "datetime.date(2020, 1, 1)",
        "2020-01-01",
    )
}

fn bytes() -> Value {
    foreign_value("bytes", "<class 'bytes'>", "b'ab'", "b'ab'")
}

fn spec(version: &str, fields: Value) -> Value {
    let mut spec = json!({
        "openapi": version,
        "info": {"title": "t", "version": "1"},
        "paths": {},
    });
    for (key, value) in fields.as_object().expect("fields") {
        spec[key] = value.clone();
    }
    spec
}

fn messages(spec: &Value, mode: ErrorMode) -> Vec<String> {
    let options = Options {
        errors: mode,
        ..Options::default()
    };
    match validate(spec, &options) {
        Outcome::Valid => Vec::new(),
        Outcome::Invalid(errors) => errors.into_iter().map(|e| e.message).collect(),
        Outcome::Error(message) => panic!("validation aborted: {message}"),
    }
}

#[test]
fn accepted_where_no_type_applies() {
    for version in ["3.0.3", "3.1.0"] {
        let spec = spec(
            version,
            json!({
                "x-when": date(),
                "components": {"schemas": {"A": {"type": "string", "example": date()}}},
            }),
        );
        assert_eq!(
            messages(&spec, ErrorMode::All),
            Vec::<String>::new(),
            "{version}"
        );
    }
}

#[test]
fn of_no_type_wherever_a_type_is_required() {
    let message = "datetime.date(2020, 1, 1) is not of type 'string'";
    for version in ["3.0.3", "3.1.0"] {
        let spec = spec(version, json!({"info": {"title": "t", "version": date()}}));
        assert_eq!(messages(&spec, ErrorMode::All), [message], "{version}");
    }
    // Swagger 2.0 reaches its `info` through the same kind of error.
    let swagger = json!({"swagger": "2.0", "info": date(), "paths": {}});
    assert_eq!(
        messages(&swagger, ErrorMode::All),
        ["datetime.date(2020, 1, 1) is not of type 'object'"]
    );
    // A list of them is a list to jsonschema, shown with their reprs.
    let spec = spec(
        "3.0.3",
        json!({"info": {"title": [date()], "version": "1"}}),
    );
    assert_eq!(
        messages(&spec, ErrorMode::First),
        ["[datetime.date(2020, 1, 1)] is not of type 'string'"]
    );
}

#[test]
fn only_the_type_error_where_a_mapping_is_required() {
    // Python's jsonschema skips `required`, `properties`, `additionalProperties`, ... for
    // anything that is not a dict: the type error is the one error.
    for version in ["3.0.3", "3.1.0"] {
        let spec = spec(version, json!({"info": date()}));
        assert_eq!(
            messages(&spec, ErrorMode::All),
            ["datetime.date(2020, 1, 1) is not of type 'object'"],
            "{version}"
        );
    }
    // Once from the meta-schema, once from the walk's own check, as upstream reports it.
    let spec = spec("3.1.0", json!({"components": {"schemas": {"A": date()}}}));
    let message = "datetime.date(2020, 1, 1) is not of type 'object', 'boolean'";
    assert_eq!(messages(&spec, ErrorMode::All), [message, message]);
}

#[test]
fn as_a_default_value() {
    let schema = |schema: Value, version: &str| {
        let key = if version == "2.0" {
            "definitions"
        } else {
            "components"
        };
        let mut spec = if version == "2.0" {
            json!({"swagger": "2.0", "info": {"title": "t", "version": "1"}, "paths": {}})
        } else {
            self::spec(version, json!({}))
        };
        spec[key] = if version == "2.0" {
            json!({"A": schema})
        } else {
            json!({"schemas": {"A": schema}})
        };
        spec
    };
    let typed = json!({"type": "string", "default": date()});
    assert_eq!(
        messages(&schema(typed.clone(), "3.0.3"), ErrorMode::All),
        ["datetime.date(2020, 1, 1) is not of type 'string'"]
    );
    assert_eq!(
        messages(&schema(typed.clone(), "3.1.0"), ErrorMode::All),
        ["datetime.date(2020, 1, 1) is not of type 'string'"]
    );
    // Upstream does not validate Swagger 2.0 defaults in `definitions`.
    assert_eq!(
        messages(&schema(typed, "2.0"), ErrorMode::All),
        Vec::<String>::new()
    );
    // Keywords that only concern mappings are skipped for it.
    let untyped =
        json!({"properties": {"a": {"type": "string"}}, "required": ["a"], "default": date()});
    for version in ["3.0.3", "3.1.0"] {
        assert_eq!(
            messages(&schema(untyped.clone(), version), ErrorMode::All),
            Vec::<String>::new(),
            "{version}"
        );
    }
    // Equal objects are equal (`enum`), different ones are not.
    let in_enum = json!({"enum": [date(), "x"], "default": date()});
    assert_eq!(
        messages(&schema(in_enum, "3.0.3"), ErrorMode::All),
        Vec::<String>::new()
    );
    let not_in_enum = json!({"enum": [bytes()], "default": date()});
    assert_eq!(
        messages(&schema(not_in_enum, "3.0.3"), ErrorMode::All),
        ["datetime.date(2020, 1, 1) is not one of [b'ab']"]
    );
    // openapi-schema-validator lets `bytes` pass as a binary string in OAS 3.0.
    let binary = json!({"type": "string", "format": "binary", "default": bytes()});
    assert_eq!(
        messages(&schema(binary, "3.0.3"), ErrorMode::All),
        Vec::<String>::new()
    );
    let text = json!({"type": "string", "default": bytes()});
    assert_eq!(
        messages(&schema(text, "3.0.3"), ErrorMode::All),
        ["b'ab' is not of type 'string'"]
    );
}

#[test]
fn rendered_with_its_repr() {
    let spec = spec("3.0.3", json!({"info": {"title": "t", "version": date()}}));
    let Outcome::Invalid(errors) = validate(&spec, &Options::default()) else {
        panic!("expected an error");
    };
    let rendered = errors[0].to_string();
    assert!(
        rendered.ends_with("On instance['info']['version']:\n    datetime.date(2020, 1, 1)"),
        "{rendered}"
    );
}
