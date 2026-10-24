//! Snapshots of the CLI's output and exit codes.
//!
//! Every snapshot is byte-identical to what openapi-spec-validator 0.9.0 prints for the same
//! arguments (checked with `scripts/compare_output.py`), so a changed snapshot is a
//! compatibility change.

use std::collections::HashMap;
use std::sync::Arc;

use openapi_spec_validator_rs::{Retrieve, RetrieveError};
use openapi_spec_validator_rs_cli::{run, run_with_stdin};

/// Runs the CLI from the crate directory with fixture paths relative to it.
fn cli(args: &[&str]) -> String {
    let args: Vec<String> = args
        .iter()
        .map(|arg| {
            if arg.ends_with(".yaml") {
                format!("tests/fixtures/{arg}")
            } else {
                (*arg).to_owned()
            }
        })
        .collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run(&args, &mut out, &mut err);
    format!(
        "exit code: {code}\n----- stdout -----\n{}----- stderr -----\n{}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    )
}

#[test]
fn valid_openapi_3() {
    insta::assert_snapshot!(cli(&["valid.yaml"]));
}

#[test]
fn valid_swagger_2() {
    insta::assert_snapshot!(cli(&["swagger.yaml"]));
}

#[test]
fn meta_schema_error_with_best_match() {
    insta::assert_snapshot!(cli(&["missing-response-description.yaml"]));
}

#[test]
fn meta_schema_error_with_all_subschema_errors() {
    insta::assert_snapshot!(cli(&[
        "--subschema-errors",
        "all",
        "missing-response-description.yaml"
    ]));
}

#[test]
fn first_error_only() {
    insta::assert_snapshot!(cli(&["many-errors.yaml"]));
}

#[test]
fn all_validation_errors() {
    insta::assert_snapshot!(cli(&["--validation-errors", "all", "many-errors.yaml"]));
}

#[test]
fn python_regex_rules() {
    insta::assert_snapshot!(cli(&["bad-regex.yaml"]));
}

#[test]
fn exception_from_path_template() {
    insta::assert_snapshot!(cli(&["crash-path-template.yaml"]));
}

#[test]
fn exception_from_unquoted_version() {
    insta::assert_snapshot!(cli(&["crash-version.yaml"]));
}

#[test]
fn yaml_syntax_error() {
    insta::assert_snapshot!(cli(&["broken.yaml"]));
}

#[test]
fn refs_across_files() {
    insta::assert_snapshot!(cli(&["split/openapi.yaml"]));
}

#[test]
fn stops_at_first_failing_file() {
    insta::assert_snapshot!(cli(&[
        "valid.yaml",
        "swagger.yaml",
        "many-errors.yaml",
        "valid.yaml"
    ]));
}

#[test]
fn explicit_schema_version() {
    insta::assert_snapshot!(cli(&["--schema", "3.1", "valid.yaml"]));
}

#[test]
fn missing_file() {
    insta::assert_snapshot!(cli(&["does-not-exist.yaml"]));
}

#[test]
fn deprecated_errors_flag_warns() {
    insta::assert_snapshot!(cli(&["--errors", "all", "valid.yaml"]));
}

#[test]
fn oas31_default_values() {
    insta::assert_snapshot!(cli(&["--validation-errors", "all", "oas31-defaults.yaml"]));
}

#[test]
fn first_error_before_an_exception() {
    insta::assert_snapshot!(cli(&["oas31-malformed-ref-target.yaml"]));
}

#[test]
fn exception_from_a_malformed_keyword() {
    insta::assert_snapshot!(cli(&[
        "--validation-errors",
        "all",
        "oas31-malformed-ref-target.yaml"
    ]));
}

#[test]
fn best_match_hint_in_python_order() {
    insta::assert_snapshot!(cli(&["swagger-parameter.yaml"]));
}

#[test]
fn meta_schema_path_through_dynamic_ref() {
    insta::assert_snapshot!(cli(&["oas31-nested-schema-error.yaml"]));
}

/// Runs the CLI on `document` from stdin, with `$ref`s to `https://example.com/...`
/// served from `remote` instead of the network (output as for a reachable server).
fn cli_remote(args: &[&str], document: &str, remote: &[(&str, &str)]) -> String {
    let documents: HashMap<String, Vec<u8>> = remote
        .iter()
        .map(|(path, body)| {
            (
                format!("https://example.com{path}"),
                body.as_bytes().to_vec(),
            )
        })
        .collect();
    let retriever: Arc<dyn Retrieve> = Arc::new(move |url: &str| {
        documents
            .get(url)
            .cloned()
            .ok_or_else(|| RetrieveError::new("not found"))
    });
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut stdin = std::io::Cursor::new(document.as_bytes().to_vec());
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let code = run_with_stdin(&args, &mut stdin, Some(retriever), &mut out, &mut err);
    format!(
        "exit code: {code}\n----- stdout -----\n{}----- stderr -----\n{}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    )
}

const REMOTE_REF: &str = "openapi: 3.0.0
info: {title: t, version: '1'}
paths:
  /u:
    get:
      responses:
        '200':
          description: ok
          content:
            application/json:
              schema:
                $ref: https://example.com/schemas/user.json
";

#[test]
fn remote_ref_valid() {
    insta::assert_snapshot!(cli_remote(
        &["-"],
        REMOTE_REF,
        &[("/schemas/user.json", r#"{"type": "object"}"#)],
    ));
}

#[test]
fn remote_ref_invalid_default() {
    insta::assert_snapshot!(cli_remote(
        &["-"],
        REMOTE_REF,
        &[(
            "/schemas/user.json",
            r#"{"type": "integer", "default": "x"}"#
        )],
    ));
}

#[test]
fn remote_ref_unresolvable() {
    insta::assert_snapshot!(cli_remote(&["-"], REMOTE_REF, &[]));
}
