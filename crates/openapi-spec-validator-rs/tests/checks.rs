//! One fixture per upstream check. Expected results were recorded by running
//! openapi-spec-validator 0.9.0 (`--validation-errors all`) on the same files.

use std::path::Path;

use openapi_spec_validator_rs::{ErrorKind, ErrorMode, Options, Outcome, validate_path};

fn run(name: &str) -> Outcome {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/checks")
        .join(format!("{name}.json"));
    let options = Options {
        errors: ErrorMode::All,
        ..Options::default()
    };
    validate_path(&path, &options).expect("fixture loads")
}

#[track_caller]
fn assert_valid(name: &str) {
    assert_eq!(run(name), Outcome::Valid, "{name}");
}

#[track_caller]
fn assert_errors(name: &str, expected: &[(ErrorKind, &str)]) {
    let Outcome::Invalid(errors) = run(name) else {
        panic!("{name}: expected validation errors");
    };
    let actual: Vec<(ErrorKind, &str)> = errors
        .iter()
        .map(|error| (error.kind, error.message.as_str()))
        .collect();
    assert_eq!(actual, expected, "{name}");
}

#[track_caller]
fn assert_crash(name: &str, expected_prefix: &str) {
    match run(name) {
        Outcome::Error(message) => assert!(
            message.starts_with(expected_prefix),
            "{name}: {message:?} does not start with {expected_prefix:?}"
        ),
        other => panic!("{name}: expected an error, got {other:?}"),
    }
}

use ErrorKind::*;

#[test]
fn document_meta_schema() {
    assert_errors(
        "meta_schema_missing_info",
        &[(OpenApiValidation, "'info' is a required property")],
    );
    assert_errors(
        "duplicate_tag",
        &[
            (
                OpenApiValidation,
                "[{'name': 'a'}, {'name': 'a'}] has non-unique elements",
            ),
            (OpenApiValidation, "Duplicate tag name 'a'"),
        ],
    );
}

#[test]
fn draft4_meta_schema_is_pythons_copy() {
    // The Draft 4 meta-schema built into the jsonschema crate drops `uniqueItems` on `enum`.
    assert_errors(
        "enum_duplicates",
        &[(
            OpenApiValidation,
            "['true', 'true'] has non-unique elements",
        )],
    );
    // Swagger 2.0's meta-schema reuses Draft 4's `enum` definition.
    assert_errors(
        "v2_enum_duplicates",
        &[(
            OpenApiValidation,
            "{'name': 'q', 'in': 'query', 'type': 'string', 'enum': ['a', 'a']} is not valid \
             under any of the given schemas",
        )],
    );
}

#[test]
fn schema_meta_check() {
    assert_errors(
        "schema_invalid_python_regex",
        &[(OpenApiValidation, "'(?<name>x)' is not a 'regex'")],
    );
    assert_errors(
        "schema_ref_to_string",
        &[(OpenApiValidation, "'t' is not of type 'object', 'boolean'")],
    );
    assert_valid("v31_boolean_schemas");
    assert_errors(
        "v31_non_string_dialect",
        &[(OpenApiValidation, "Unknown JSON Schema dialect: 5")],
    );
    assert_errors(
        "v31_unknown_dialect",
        &[(
            OpenApiValidation,
            "Unknown JSON Schema dialect: 'https://example.com/dialect'",
        )],
    );
}

#[test]
fn required_properties() {
    assert_errors(
        "required_undefined_with_allof",
        &[(
            ExtraParameters,
            "Required list has not defined properties: ['zz']",
        )],
    );
    assert_valid("required_undefined_without_allof");
    assert_valid("required_defined_in_allof_member");
    // First visit wins: reached as a nested property first, so never checked.
    assert_valid("required_first_visit_nested");
}

#[test]
fn default_values() {
    assert_errors(
        "default_wrong_type",
        &[(OpenApiValidation, "5 is not of type 'string'")],
    );
    assert_errors(
        "default_null_not_nullable",
        &[
            (OpenApiValidation, "None for not nullable"),
            (OpenApiValidation, "None is not of type 'string'"),
        ],
    );
    assert_valid("default_null_nullable");
    assert_errors(
        "default_via_ref_in_property",
        &[(OpenApiValidation, "'x' is not of type 'integer'")],
    );
    // `$ref` siblings are invisible to the walk.
    assert_valid("default_ref_sibling_ignored");
    assert_errors(
        "default_enum_mismatch",
        &[(OpenApiValidation, "'c' is not one of ['a', 'b']")],
    );
    assert_errors(
        "default_format_int32_overflow",
        &[(OpenApiValidation, "3000000000 is not a 'int32'")],
    );
    assert_valid("default_readonly_required_skipped");
    assert_errors(
        "v31_default_wrong_type",
        &[(OpenApiValidation, "'x' is not of type 'integer'")],
    );
    assert_errors(
        "v2_parameter_default",
        &[(OpenApiValidation, "'x' is not of type 'integer'")],
    );
}

#[test]
fn walk_coverage_matches_upstream() {
    // Upstream does not walk Swagger 2.0 definitions or request bodies directly.
    assert_valid("v2_definitions_not_walked");
    assert_valid("request_body_not_walked");
    assert_valid("valid_petstore_like");
}

#[test]
fn parameters_and_operations() {
    assert_errors(
        "duplicate_parameter",
        &[(ParameterDuplicate, "Duplicate parameter 'q'")],
    );
    assert_errors(
        "duplicate_operation_id",
        &[(
            DuplicateOperationId,
            "Operation ID 'x' for 'get' in '/b' is not unique",
        )],
    );
    assert_errors(
        "path_param_not_declared",
        &[(
            UnresolvableParameter,
            "Path parameter 'id' for 'get' operation in '/a/{id}' was not resolved",
        )],
    );
    assert_errors(
        "path_param_not_in_url",
        &[(
            UnresolvableParameter,
            "Path parameter 'id' for 'get' operation in '/a' was not resolved",
        )],
    );
    assert_valid("path_param_declared_on_path_item");
    assert_errors(
        "v32_query_operation_path_params",
        &[(
            UnresolvableParameter,
            "Path parameter 'id' for 'query' operation in '/a/{id}' was not resolved",
        )],
    );
    assert_errors(
        "v32_additional_operations",
        &[(
            UnresolvableParameter,
            "Path parameter 'id' for 'LINK' operation in '/a/{id}' was not resolved",
        )],
    );
}

#[test]
fn tags() {
    assert_errors(
        "v32_tag_unknown_parent",
        &[(
            OpenApiValidation,
            "Tag 'a' references unknown parent tag 'zz'",
        )],
    );
    assert_errors(
        "v32_tag_parent_cycle",
        &[
            (
                OpenApiValidation,
                "Circular tag hierarchy detected: a -> b -> a",
            ),
            (
                OpenApiValidation,
                "Circular tag hierarchy detected: b -> a -> b",
            ),
        ],
    );
}

#[test]
fn upstream_exceptions() {
    assert_crash(
        "path_template_single_brace",
        "Single '}' encountered in format string",
    );
    assert_crash("ref_cycle", "maximum recursion depth exceeded");
    assert_crash(
        "collect_properties_cycle",
        "maximum recursion depth exceeded",
    );
    assert_crash(
        "unresolvable_ref",
        "'/components/schemas/Nope' does not exist within {'openapi': '3.0.3', 'info': {",
    );
    assert_crash(
        "version_float",
        "expected string or bytes-like object, got 'float'",
    );
    assert_eq!(run("version_unknown"), Outcome::Error(String::new()));
    assert_crash(
        "ref_anchor_missing_slash",
        "'#components/schemas/X' is not a valid anchor, neither as a plain name anchor nor as a \
         JSON Pointer. You may have intended to use '#/components/schemas/X'",
    );
    assert_eq!(
        run("ref_missing_file"),
        Outcome::Error("./missing.json#/X".to_owned())
    );
    // urllib opens `file:///...json?x=1` as a file name of its own, which does not exist.
    assert_eq!(
        run("ref_with_query"),
        Outcome::Error("?x=1#/components/schemas/B".to_owned())
    );
    assert_crash(
        "discriminator_mapping_not_object",
        "'str' object has no attribute 'get'",
    );
}

#[test]
fn unhashable_parameter_names_are_never_duplicates() {
    let invalid = "{'name': {'x': 1}, 'in': 'query', 'schema': {'type': 'string'}} is not valid \
                   under any of the given schemas";
    assert_errors(
        "parameter_name_mapping",
        &[
            (OpenApiValidation, invalid),
            (OpenApiValidation, invalid),
            (
                OpenApiValidation,
                "[{'name': {'x': 1}, 'in': 'query', 'schema': {'type': 'string'}}, {'name': \
                 {'x': 1}, 'in': 'query', 'schema': {'type': 'string'}}] has non-unique elements",
            ),
        ],
    );
}

#[test]
fn float_multiple_of_in_31_defaults() {
    assert_errors(
        "v31_default_float_multiple_of",
        &[(OpenApiValidation, "0.3 is not a multiple of 0.1")],
    );
}
