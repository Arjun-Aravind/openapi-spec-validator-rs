//! `http://` `$ref`s against a local HTTP server. Documents are fetched when a reference
//! to them is first followed, once each, parsed like files, and any failure makes the
//! reference unresolvable, as with openapi-spec-validator 0.9.0 (measured against it).

#![cfg(feature = "http")]

use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use openapi_spec_validator_rs::{
    ErrorMode, HttpRetriever, Options, Outcome, Retrieve, RetrieveError, validate,
};
use serde_json::{Value, json};

mod support;

use support::{Reply, Server, body};

/// An OpenAPI 3.0 document whose only response schema is `$ref: reference`.
fn spec(reference: &str) -> Value {
    json!({
        "openapi": "3.0.0",
        "info": {"title": "t", "version": "1"},
        "paths": {"/u": {"get": {"responses": {"200": {
            "description": "ok",
            "content": {"application/json": {"schema": {"$ref": reference}}},
        }}}}},
    })
}

fn user() -> Value {
    json!({"type": "object", "properties": {"name": {"type": "string"}}})
}

fn bad_default() -> Value {
    json!({"type": "integer", "default": "x"})
}

/// The built-in client, ignoring any proxy configured in the environment (which would
/// capture the requests to the test server; `http_env.rs` covers proxies).
fn retriever(timeout: Duration) -> Option<Arc<dyn Retrieve>> {
    Some(Arc::new(
        HttpRetriever::with_timeout(timeout).without_proxies(),
    ))
}

fn run(spec: &Value, retriever: Option<Arc<dyn Retrieve>>) -> Outcome {
    validate(
        spec,
        &Options {
            retriever,
            ..Options::default()
        },
    )
}

fn messages(outcome: Outcome) -> Vec<String> {
    match outcome {
        Outcome::Invalid(errors) => errors.into_iter().map(|error| error.message).collect(),
        other => panic!("expected validation errors, got {other:?}"),
    }
}

#[test]
fn remote_documents_are_fetched_and_validated() {
    let server = Server::start(vec![
        ("/user.json", body(&user())),
        ("/bad-default.json", body(&bad_default())),
        (
            "/doc.yaml",
            Reply::Body(
                b"openapi: 3.0.0\ninfo: {title: R, version: '1'}\npaths: {}\ncomponents:\n  \
                  schemas:\n    User: {type: object, properties: {id: {type: integer, default: oops}}}\n"
                    .to_vec(),
            ),
        ),
    ]);
    assert_eq!(
        run(
            &spec(&server.url("/user.json")),
            Some(Arc::new(HttpRetriever::new().without_proxies()))
        ),
        Outcome::Valid
    );
    assert_eq!(
        messages(run(
            &spec(&server.url("/bad-default.json")),
            retriever(TIMEOUT)
        )),
        ["'x' is not of type 'integer'"]
    );
    assert_eq!(
        messages(run(
            &spec(&server.url("/doc.yaml#/components/schemas/User")),
            retriever(TIMEOUT)
        )),
        ["'oops' is not of type 'integer'"]
    );
    assert_eq!(
        server.hits(),
        ["/user.json", "/bad-default.json", "/doc.yaml"]
    );
}

const TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn relative_references_resolve_against_the_remote_document() {
    let server = Server::start(vec![
        (
            "/nested.json",
            body(&json!({"type": "object", "properties": {"deep": {"$ref": "user-rel.json"}}})),
        ),
        (
            "/user-rel.json",
            body(
                &json!({"type": "object", "properties": {"address": {"$ref": "schemas/address.json"}}}),
            ),
        ),
        (
            "/schemas/address.json",
            body(
                &json!({"type": "object", "properties": {"zip": {"type": "integer", "default": "notint"}}}),
            ),
        ),
    ]);
    assert_eq!(
        messages(run(&spec(&server.url("/nested.json")), retriever(TIMEOUT))),
        ["'notint' is not of type 'integer'"]
    );
    assert_eq!(
        server.hits(),
        ["/nested.json", "/user-rel.json", "/schemas/address.json"]
    );
}

#[test]
fn query_strings_are_part_of_the_url() {
    let server = Server::start(vec![("/bad-default.json", body(&bad_default()))]);
    assert_eq!(
        messages(run(
            &spec(&server.url("/bad-default.json?v=1")),
            retriever(TIMEOUT)
        )),
        ["'x' is not of type 'integer'"]
    );
    assert_eq!(server.hits(), ["/bad-default.json?v=1"]);
}

#[test]
fn each_document_is_fetched_once_and_only_when_reached() {
    let server = Server::start(vec![
        ("/user.json", body(&user())),
        ("/bad-default.json", body(&bad_default())),
    ]);
    let mut document = spec(&server.url("/user.json"));
    document["components"] = json!({"schemas": {
        "A": {"$ref": server.url("/user.json")},
        "B": {"$ref": server.url("/user.json")},
        // Upstream does not resolve references inside examples ...
        "C": {"type": "object", "example": {"$ref": server.url("/bad-default.json")}},
    }});
    // ... or inside extensions.
    document["info"]["x-ext"] = json!({"$ref": server.url("/bad-default.json")});
    assert_eq!(run(&document, retriever(TIMEOUT)), Outcome::Valid);
    assert_eq!(server.hits(), ["/user.json"]);
}

#[test]
fn nothing_is_fetched_after_the_first_error() {
    let server = Server::start(vec![("/bad-default.json", body(&bad_default()))]);
    let document = json!({
        "openapi": "3.0.0",
        "info": {"title": "t", "version": "1"},
        "paths": {"/u": {"get": {
            "parameters": [{"name": "q", "in": "query", "schema": {"type": "integer", "default": "local"}}],
            "responses": {"200": {"description": "ok"}},
        }}},
        "components": {"schemas": {"R": {"$ref": server.url("/bad-default.json")}}},
    });
    assert_eq!(
        messages(run(&document, retriever(TIMEOUT))),
        ["'local' is not of type 'integer'"]
    );
    assert_eq!(server.hits(), Vec::<String>::new());

    let all = Options {
        errors: ErrorMode::All,
        retriever: retriever(TIMEOUT),
        ..Options::default()
    };
    assert_eq!(
        messages(validate(&document, &all)),
        [
            "'local' is not of type 'integer'",
            "'x' is not of type 'integer'"
        ]
    );
    assert_eq!(server.hits(), ["/bad-default.json"]);
}

#[test]
fn remote_documents_reached_from_default_values_and_discriminators() {
    let server = Server::start(vec![
        ("/int.json", body(&json!({"type": "integer"}))),
        (
            "/param.json",
            body(&json!({"name": "q", "in": "query", "type": "integer", "default": "badparam"})),
        ),
    ]);
    for version in ["3.0.0", "3.1.0"] {
        let document = json!({
            "openapi": version,
            "info": {"title": "t", "version": "1"},
            "paths": {},
            "components": {"schemas": {"A": {
                "type": "object",
                "properties": {"x": {"$ref": server.url("/int.json")}},
                "default": {"x": "str"},
            }}},
        });
        assert_eq!(
            messages(run(&document, retriever(TIMEOUT))),
            ["'str' is not of type 'integer'"],
            "{version}"
        );
    }
    let discriminator = json!({
        "openapi": "3.0.0",
        "info": {"title": "t", "version": "1"},
        "paths": {},
        "components": {"schemas": {
            "Pet": {
                "type": "object",
                "properties": {"kind": {"type": "string"}},
                "discriminator": {"propertyName": "kind", "mapping": {"dog": server.url("/int.json")}},
                "oneOf": [{"$ref": "#/components/schemas/Dog"}],
                "default": {"kind": "dog"},
            },
            "Dog": {"type": "object"},
        }},
    });
    assert_eq!(
        messages(run(&discriminator, retriever(TIMEOUT))),
        ["{'kind': 'dog'} is not of type 'integer'"]
    );
    let swagger = json!({
        "swagger": "2.0",
        "info": {"title": "t", "version": "1"},
        "paths": {"/u": {"get": {
            "parameters": [{"$ref": server.url("/param.json")}],
            "responses": {"200": {"description": "ok"}},
        }}},
    });
    assert_eq!(
        messages(run(&swagger, retriever(TIMEOUT))),
        ["'badparam' is not of type 'integer'"]
    );
}

#[test]
fn redirects_are_followed() {
    let server = Server::start(vec![
        ("/redirect", Reply::Redirect("/user.json".to_owned())),
        ("/user.json", body(&user())),
    ]);
    assert_eq!(
        run(&spec(&server.url("/redirect")), retriever(TIMEOUT)),
        Outcome::Valid
    );
    assert_eq!(server.hits(), ["/redirect", "/user.json"]);
}

#[test]
fn every_failure_makes_the_reference_unresolvable() {
    let server = Server::start(vec![
        ("/500", Reply::Status(500)),
        ("/loop", Reply::Redirect("/loop".to_owned())),
        ("/bad.yaml", Reply::Body(b"a: [\n".to_vec())),
        (
            "/latin1.yaml",
            Reply::Body(b"type: string\ndescription: caf\xe9\n".to_vec()),
        ),
    ]);
    let unused = TcpListener::bind("127.0.0.1:0").expect("a local port is free");
    let refused = format!("http://{}/user.json", unused.local_addr().expect("bound"));
    drop(unused);
    let https = server.url("/user.json").replacen("http://", "https://", 1);
    for reference in [
        server.url("/missing.json"),
        server.url("/500"),
        server.url("/loop"),
        server.url("/bad.yaml"),
        server.url("/latin1.yaml"),
        refused,
        https,
        "ftp://127.0.0.1/user.json".to_owned(),
    ] {
        assert_eq!(
            run(&spec(&reference), retriever(TIMEOUT)),
            Outcome::Error(reference.clone()),
            "{reference}"
        );
    }
}

#[test]
fn slow_servers_time_out() {
    let server = Server::start(vec![
        ("/slow", Reply::Slow(Duration::from_secs(3))),
        ("/stall", Reply::Stall(Duration::from_secs(3))),
    ]);
    for path in ["/slow", "/stall"] {
        let started = Instant::now();
        assert_eq!(
            run(
                &spec(&server.url(path)),
                retriever(Duration::from_millis(300))
            ),
            Outcome::Error(server.url(path))
        );
        assert!(started.elapsed() < Duration::from_secs(2), "{path}");
    }
}

#[test]
fn large_documents_are_read_whole() {
    let mut schema = bad_default();
    schema["description"] = Value::String("x".repeat(11 << 20));
    let server = Server::start(vec![("/large.json", body(&schema))]);
    assert_eq!(
        messages(run(&spec(&server.url("/large.json")), retriever(TIMEOUT))),
        ["'x' is not of type 'integer'"]
    );
}

#[test]
fn without_a_retriever_remote_references_are_unresolvable() {
    let server = Server::start(vec![("/user.json", body(&user()))]);
    let reference = server.url("/user.json");
    assert_eq!(run(&spec(&reference), None), Outcome::Error(reference));
    assert_eq!(server.hits(), Vec::<String>::new());
}

#[test]
fn custom_retrievers_are_called_with_the_url() {
    let documents: HashMap<String, Vec<u8>> = HashMap::from([(
        "https://example.com/schemas/user.json?v=2".to_owned(),
        user().to_string().into_bytes(),
    )]);
    let retriever = Arc::new(move |url: &str| {
        documents
            .get(url)
            .cloned()
            .ok_or_else(|| RetrieveError::new(format!("no such document: {url}")))
    });
    assert_eq!(
        run(
            &spec("https://example.com/schemas/user.json?v=2"),
            Some(retriever.clone())
        ),
        Outcome::Valid
    );
    assert_eq!(
        run(&spec("https://example.com/other.json"), Some(retriever)),
        Outcome::Error("https://example.com/other.json".to_owned())
    );
}

#[test]
fn the_default_options_fetch_remote_references() {
    assert!(Options::default().retriever.is_some());
    assert!(format!("{:?}", Options::default()).contains("retriever: Some"));
}

#[test]
fn urls_are_requested_as_written_and_refused_as_urllib_refuses_them() {
    let server = Server::start(vec![
        ("/sub/../bad.json", body(&bad_default())),
        ("/a b.json", body(&user())),
        ("/user.json", body(&user())),
    ]);
    // Dot segments in an absolute reference are sent verbatim (`urljoin` leaves an
    // absolute URL alone), and a space is refused before any request, as urllib does.
    assert_eq!(
        messages(run(
            &spec(&server.url("/sub/../bad.json")),
            retriever(TIMEOUT)
        )),
        ["'x' is not of type 'integer'"]
    );
    let space = server.url("/a b.json");
    assert_eq!(
        run(&spec(&space), retriever(TIMEOUT)),
        Outcome::Error(space)
    );
    let userinfo = format!("http://u:p@{}/user.json", server.authority());
    assert_eq!(
        run(&spec(&userinfo), retriever(TIMEOUT)),
        Outcome::Error(userinfo)
    );
    assert_eq!(server.hits(), ["/sub/../bad.json"]);
}

#[test]
fn redirects_follow_urllib_rules() {
    let server = Server::start(vec![
        ("/user.json", body(&user())),
        ("/loop", Reply::Redirect("/loop".to_owned())),
        ("/to-file", Reply::Redirect("file:///etc/hosts".to_owned())),
        ("/no-location", Reply::Status(302)),
        ("/c1", Reply::Redirect("/c2".to_owned())),
        ("/c2", Reply::Redirect("/c3".to_owned())),
        ("/c3", Reply::Redirect("/c4".to_owned())),
        ("/c4", Reply::Redirect("/c5".to_owned())),
        ("/c5", Reply::Redirect("/c6".to_owned())),
        ("/c6", Reply::Redirect("/c7".to_owned())),
        ("/c7", Reply::Redirect("/c8".to_owned())),
        ("/c8", Reply::Redirect("/c9".to_owned())),
        ("/c9", Reply::Redirect("/c10".to_owned())),
        ("/c10", Reply::Redirect("/c11".to_owned())),
        ("/c11", Reply::Redirect("/user.json".to_owned())),
        ("/spaced", Reply::Redirect("/user .json".to_owned())),
        ("/user%20.json", body(&user())),
        ("/304", Reply::Status(304)),
    ]);
    // The same URL is visited at most four times: five requests, as urllib makes.
    let reference = server.url("/loop");
    assert_eq!(
        run(&spec(&reference), retriever(TIMEOUT)),
        Outcome::Error(reference)
    );
    assert_eq!(server.hits(), ["/loop"; 5]);
    // A redirect to another scheme is refused without a request.
    let reference = server.url("/to-file");
    assert_eq!(
        run(&spec(&reference), retriever(TIMEOUT)),
        Outcome::Error(reference)
    );
    assert_eq!(server.hits().len(), 6);
    // A redirect without a Location is an error status.
    let reference = server.url("/no-location");
    assert_eq!(
        run(&spec(&reference), retriever(TIMEOUT)),
        Outcome::Error(reference)
    );
    // Ten redirections are followed, the eleventh is not.
    let reference = server.url("/c1");
    assert_eq!(
        run(&spec(&reference), retriever(TIMEOUT)),
        Outcome::Error(reference)
    );
    assert_eq!(server.hits().len(), 7 + 11);
    // A space in a Location is percent-encoded, as urllib does.
    assert_eq!(
        run(&spec(&server.url("/spaced")), retriever(TIMEOUT)),
        Outcome::Valid
    );
    assert_eq!(
        server.hits().last().map(String::as_str),
        Some("/user%20.json")
    );
    // Only 2xx is success: urllib raises for a 304.
    let reference = server.url("/304");
    assert_eq!(
        run(&spec(&reference), retriever(TIMEOUT)),
        Outcome::Error(reference)
    );
}

#[test]
fn a_body_cut_short_is_used_as_far_as_it_arrived() {
    let partial = bad_default().to_string().into_bytes();
    let server = Server::start(vec![(
        "/truncated.json",
        Reply::Truncated(partial.clone(), partial.len() + 1000),
    )]);
    assert_eq!(
        messages(run(
            &spec(&server.url("/truncated.json")),
            retriever(TIMEOUT)
        )),
        ["'x' is not of type 'integer'"]
    );
}
