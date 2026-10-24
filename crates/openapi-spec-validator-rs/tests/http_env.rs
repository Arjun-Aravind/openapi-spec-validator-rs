//! Proxy environment variables, read as `urllib` reads them. This is its own test
//! binary because the variables are process-wide.

#![cfg(feature = "http")]

mod support;

use std::sync::Arc;
use std::time::Duration;

use openapi_spec_validator_rs::{HttpRetriever, Options, Outcome, Retrieve, validate};
use serde_json::{Value, json};
use support::{Server, body};

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

fn run(reference: &str) -> Outcome {
    validate(
        &spec(reference),
        &Options {
            retriever: Some(Arc::new(HttpRetriever::with_timeout(Duration::from_secs(
                5,
            )))),
            ..Options::default()
        },
    )
}

fn clear_env() {
    for name in [
        "http_proxy",
        "HTTP_PROXY",
        "https_proxy",
        "HTTPS_PROXY",
        "all_proxy",
        "ALL_PROXY",
        "no_proxy",
        "NO_PROXY",
    ] {
        // SAFETY: this test binary is single-threaded while the environment changes.
        unsafe { std::env::remove_var(name) };
    }
}

fn set(name: &str, value: &str) {
    // SAFETY: as in `clear_env`.
    unsafe { std::env::set_var(name, value) };
}

#[test]
fn proxies_come_from_the_environment_like_urllib() {
    // The "proxy" is the same server, which tunnels CONNECT requests to itself.
    let server = Server::start(vec![("/user.json", body(&json!({"type": "object"})))]);
    let origin = server.url("/user.json");
    // A proxied fetch records a CONNECT and then the tunnelled GET; a direct one only
    // the GET. The log accumulates across the cases below.
    let proxied =
        |lines: &[String]| lines.len() >= 2 && lines[lines.len() - 2].starts_with("CONNECT ");

    clear_env();
    set("http_proxy", &server.url(""));
    HttpRetriever::with_timeout(Duration::from_secs(5))
        .retrieve(&origin)
        .expect("fetched through the proxy");
    assert!(
        proxied(&server.request_lines()),
        "{:?}",
        server.request_lines()
    );
    assert_eq!(run(&origin), Outcome::Valid);
    assert!(
        proxied(&server.request_lines()),
        "{:?}",
        server.request_lines()
    );

    // `ALL_PROXY` is not read by urllib.
    clear_env();
    set("ALL_PROXY", "http://127.0.0.1:1");
    assert_eq!(run(&origin), Outcome::Valid);
    assert!(!proxied(&server.request_lines()));

    // `HTTPS_PROXY` does not apply to http URLs.
    clear_env();
    set("HTTPS_PROXY", "http://127.0.0.1:1");
    assert_eq!(run(&origin), Outcome::Valid);
    assert!(!proxied(&server.request_lines()));

    // `no_proxy` bypasses a proxy that would otherwise refuse the connection.
    clear_env();
    set("http_proxy", "http://127.0.0.1:1");
    set("no_proxy", "127.0.0.1");
    assert_eq!(run(&origin), Outcome::Valid);
    assert!(!proxied(&server.request_lines()));
    clear_env();
    set("http_proxy", "http://127.0.0.1:1");
    assert_eq!(run(&origin), Outcome::Error(origin.clone()));
    clear_env();
}
