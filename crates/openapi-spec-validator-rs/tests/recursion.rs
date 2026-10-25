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

/// A schema nested `depth` levels deep through `wrap`, around `leaf`.
fn nested(depth: usize, wrap: &str, leaf: Value) -> Value {
    (0..depth).fold(leaf, |inner, _| match wrap {
        "properties" => json!({"type": "object", "properties": {"p": inner}}),
        "allOf" => json!({"allOf": [inner]}),
        "items" => json!({"type": "array", "items": inner}),
        _ => unreachable!(),
    })
}

fn document(version: &str, schema: Value, in_response: bool) -> Value {
    let swagger = version == "2.0";
    let mut spec = if swagger {
        json!({"swagger": "2.0", "info": {"title": "t", "version": "1"}, "paths": {}})
    } else {
        json!({"openapi": version, "info": {"title": "t", "version": "1"}, "paths": {}})
    };
    match (in_response, swagger) {
        (false, true) => spec["definitions"] = json!({"A": schema}),
        (false, false) => spec["components"] = json!({"schemas": {"A": schema}}),
        (true, true) => {
            spec["paths"] = json!({"/a": {"get": {"responses": {"200": {"description": "ok", "schema": schema}}}}});
        }
        (true, false) => {
            spec["paths"] = json!({"/a": {"get": {"responses": {"200": {"description": "ok", "content": {"application/json": {"schema": schema}}}}}}});
        }
    }
    spec
}

fn reports_recursion(spec: &Value, errors: ErrorMode) -> bool {
    let options = Options {
        errors,
        ..Options::default()
    };
    matches!(validate(spec, &options), Outcome::Error(message) if message == RECURSION)
}

/// The first nesting depth at which upstream's command line reports `RecursionError`
/// (openapi-spec-validator 0.9.0, CPython 3.13, console script), for schemas nested
/// through the meta-schema check of the document (2.0, 3.0) or of each schema (3.1).
#[test]
fn nested_schemas_against_the_meta_schemas() {
    let cases = [
        // version, wrap, leaf, in a response, mode, first depth upstream gives up at
        (
            "2.0",
            "properties",
            json!({"type": "string"}),
            false,
            ErrorMode::First,
            162,
        ),
        (
            "2.0",
            "items",
            json!({"type": "string"}),
            true,
            ErrorMode::All,
            159,
        ),
        (
            "3.0",
            "properties",
            json!({"type": "string"}),
            false,
            ErrorMode::First,
            122,
        ),
        (
            "3.0",
            "properties",
            json!({"type": 5}),
            false,
            ErrorMode::First,
            122,
        ),
        (
            "3.0",
            "allOf",
            json!({"type": "string"}),
            true,
            ErrorMode::All,
            120,
        ),
        (
            "3.0",
            "items",
            json!({"type": "string"}),
            false,
            ErrorMode::First,
            163,
        ),
        (
            "3.1.0",
            "properties",
            json!({"type": "string"}),
            false,
            ErrorMode::First,
            69,
        ),
        (
            "3.1.0",
            "allOf",
            json!({"type": "string"}),
            false,
            ErrorMode::First,
            61,
        ),
        (
            "3.1.0",
            "items",
            json!({"type": "string"}),
            true,
            ErrorMode::All,
            81,
        ),
    ];
    for (version, wrap, leaf, in_response, errors, limit) in cases {
        let version = if version == "3.0" { "3.0.3" } else { version };
        let build = |depth| document(version, nested(depth, wrap, leaf.clone()), in_response);
        assert!(
            !reports_recursion(&build(limit - 1), errors),
            "{version} {wrap} {errors:?}: {} levels",
            limit - 1
        );
        assert!(
            reports_recursion(&build(limit), errors),
            "{version} {wrap} {errors:?}: {limit} levels"
        );
    }
}

/// An error upstream yields before it gets too deep is reported, first error only.
#[test]
fn errors_before_the_depth_limit_come_first() {
    let mut spec = document(
        "3.0.3",
        nested(400, "properties", json!({"type": "string"})),
        false,
    );
    spec["info"]["title"] = json!(5);
    let options = Options::default();
    let Outcome::Invalid(errors) = validate(&spec, &options) else {
        panic!("expected the title's error");
    };
    assert_eq!(errors[0].message, "5 is not of type 'string'");
    // Every error collected: the check gets that deep anyway.
    assert!(reports_recursion(&spec, ErrorMode::All));
}

/// Far beyond the limit, the result comes at once: the error tree of the deep part is
/// never built (it grew quadratically, 1.7 GB for a 41 KB file 1,000 levels deep).
#[test]
fn far_too_deep_documents_fail_fast() {
    // Building and dropping a value 2,000 objects deep needs more than a test thread's
    // stack in a debug build.
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            for errors in [ErrorMode::First, ErrorMode::All] {
                let spec = document(
                    "3.0.3",
                    nested(1000, "properties", json!({"type": 5})),
                    false,
                );
                let started = std::time::Instant::now();
                assert!(reports_recursion(&spec, errors));
                let elapsed = started.elapsed();
                assert!(
                    elapsed < std::time::Duration::from_secs(2),
                    "{errors:?}: {elapsed:?}"
                );
            }
        })
        .expect("thread")
        .join()
        .expect("passes");
}
