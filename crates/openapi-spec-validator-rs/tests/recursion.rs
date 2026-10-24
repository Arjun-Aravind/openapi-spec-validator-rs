//! Where Python's recursion limit stops upstream. The thresholds were measured with
//! openapi-spec-validator 0.9.0 on documents built like the ones below.

use openapi_spec_validator_rs::{ErrorMode, Options, Outcome, validate};
use serde_json::{Map, Value, json};

const RECURSION: &str = "maximum recursion depth exceeded";

fn spec(version: &str, schemas: Map<String, Value>) -> Value {
    json!({
        "openapi": version,
        "info": {"title": "t", "version": "1"},
        "paths": {},
        "components": {"schemas": schemas},
    })
}

/// `S0` is an integer schema and every `Si` refers to `S(i-1)`.
fn chain(length: usize) -> Map<String, Value> {
    (0..length)
        .map(|i| {
            let schema = if i == 0 {
                json!({"type": "integer"})
            } else {
                json!({"$ref": format!("#/components/schemas/S{}", i - 1)})
            };
            (format!("S{i}"), schema)
        })
        .collect()
}

fn outcome(spec: &Value) -> Outcome {
    let options = Options {
        errors: ErrorMode::All,
        ..Options::default()
    };
    validate(spec, &options)
}

#[track_caller]
fn assert_limit(build: impl Fn(usize) -> Value, last_ok: usize) {
    assert_eq!(outcome(&build(last_ok)), Outcome::Valid, "{last_ok}");
    assert_eq!(
        outcome(&build(last_ok + 1)),
        Outcome::Error(RECURSION.to_owned()),
        "{}",
        last_ok + 1
    );
}

#[test]
fn ref_chains_while_walking() {
    assert_limit(
        |length| {
            let mut schemas = chain(length);
            let last = format!("#/components/schemas/S{}", length - 1);
            schemas.insert("X".to_owned(), json!({"$ref": last}));
            spec("3.1.0", schemas)
        },
        977,
    );
}

#[test]
fn ref_chains_under_a_default() {
    for version in ["3.0.3", "3.1.0"] {
        assert_limit(
            |length| {
                let mut schemas = chain(length);
                let last = format!("#/components/schemas/S{}", length - 1);
                schemas.insert(
                    "X".to_owned(),
                    json!({
                        "type": "object",
                        "properties": {"p": {"$ref": last}},
                        "default": {"p": 1},
                    }),
                );
                spec(version, schemas)
            },
            489,
        );
    }
}

#[test]
fn recursive_schemas_under_a_deep_default() {
    for version in ["3.0.3", "3.1.0"] {
        assert_limit(
            |depth| {
                let mut default = json!(1);
                for _ in 0..depth {
                    default = json!({"a": default});
                }
                let node = json!({"properties": {"a": {"$ref": "#/components/schemas/N"}}});
                let mut root = node.clone();
                root["default"] = default;
                spec(
                    version,
                    Map::from_iter([("N".to_owned(), node), ("X".to_owned(), root)]),
                )
            },
            245,
        );
    }
}
