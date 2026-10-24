//! `http://` `$ref`s against a local HTTP server. Documents are fetched when a reference
//! to them is first followed, once each, parsed like files, and any failure makes the
//! reference unresolvable, as with openapi-spec-validator 0.9.0 (measured against it).

#![cfg(feature = "http")]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use openapi_spec_validator_rs::{
    ErrorMode, HttpRetriever, Options, Outcome, Retrieve, RetrieveError, validate,
};
use serde_json::{Value, json};

/// What the server answers for a path.
#[derive(Clone)]
enum Reply {
    Body(Vec<u8>),
    Status(u16),
    Redirect(String),
    /// Waits before sending anything.
    Slow(Duration),
    /// Sends the headers of a longer body, then nothing more.
    Stall(Duration),
}

fn body(value: &Value) -> Reply {
    Reply::Body(value.to_string().into_bytes())
}

struct Server {
    base: String,
    hits: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn start(routes: Vec<(&'static str, Reply)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local port is free");
        let base = format!("http://{}", listener.local_addr().expect("bound"));
        let hits = Arc::new(Mutex::new(Vec::new()));
        let routes: Arc<HashMap<&str, Reply>> = Arc::new(routes.into_iter().collect());
        let seen = Arc::clone(&hits);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let routes = Arc::clone(&routes);
                let seen = Arc::clone(&seen);
                thread::spawn(move || serve(stream, &routes, &seen));
            }
        });
        Self { base, hits }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// The request targets so far, in order.
    fn hits(&self) -> Vec<String> {
        self.hits.lock().expect("not poisoned").clone()
    }
}

fn serve(mut stream: TcpStream, routes: &HashMap<&str, Reply>, hits: &Mutex<Vec<String>>) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut line = String::new();
    while reader.read_line(&mut line).is_ok() && line != "\r\n" && !line.is_empty() {
        line.clear();
    }
    let target = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_owned();
    hits.lock().expect("not poisoned").push(target.clone());
    let path = target.split('?').next().unwrap_or("");
    let (status, extra, payload): (u16, String, Vec<u8>) = match routes.get(path) {
        None => (404, String::new(), b"<html>nope</html>".to_vec()),
        Some(Reply::Body(bytes)) => (200, String::new(), bytes.clone()),
        Some(Reply::Status(status)) => (*status, String::new(), b"boom".to_vec()),
        Some(Reply::Redirect(to)) => (302, format!("Location: {to}\r\n"), Vec::new()),
        Some(Reply::Slow(delay)) => {
            thread::sleep(*delay);
            (200, String::new(), b"{}".to_vec())
        }
        Some(Reply::Stall(delay)) => {
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{{");
            let _ = stream.flush();
            thread::sleep(*delay);
            return;
        }
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
        payload.len()
    );
    let _ = stream.write_all(&payload);
}

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

fn retriever(timeout: Duration) -> Option<Arc<dyn Retrieve>> {
    Some(Arc::new(HttpRetriever::with_timeout(timeout)))
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
            Some(Arc::new(HttpRetriever::new()))
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
