//! Validation on a thread whose stack the library did not size, as when no thread of its
//! own can be started: documents nest only as deeply as that stack allows, and deeper ones
//! fail with an error instead of overflowing it.

use openapi_spec_validator_rs::{
    LoadError, Options, Outcome, STACK_TOO_SMALL, load, nesting_limit, validate, within_this_stack,
};
use serde_json::{Value, json};

fn spec(depth: usize) -> Value {
    let mut deep = json!("leaf");
    for _ in 0..depth {
        deep = json!([deep]);
    }
    json!({
        "openapi": "3.0.3",
        "info": {"title": "t", "version": "1"},
        "paths": {},
        "x-deep": deep,
    })
}

fn on_small_thread<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(1 << 20)
            .spawn_scoped(scope, || within_this_stack(f))
            .expect("thread")
            .join()
            .expect("no overflow")
    })
}

/// How deeply documents may nest on a fresh thread with a `stack_size` stack.
fn nesting_allowed(stack_size: usize) -> Option<usize> {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(stack_size)
            .spawn_scoped(scope, || within_this_stack(nesting_limit))
            .expect("thread")
            .join()
            .expect("no overflow")
    })
}

#[test]
fn the_nesting_allowed_follows_what_is_left_of_the_stack() {
    // 2 KiB per level, after a 512 KiB reserve and what the thread already uses (the
    // platform may round the size up a little).
    let small = nesting_allowed(1 << 20).expect("a limit");
    assert!((150..=300).contains(&small), "{small}");
    let large = nesting_allowed(16 << 20).expect("a limit");
    assert!((7_500..=8_000).contains(&large), "{large}");
}

#[test]
fn shallow_documents_validate_on_a_small_stack() {
    let spec = spec(20);
    assert_eq!(
        on_small_thread(|| validate(&spec, &Options::default())),
        Outcome::Valid
    );
}

/// Runs `f` where a deep document can be built and dropped (dropping recurses).
fn on_large_thread(f: impl FnOnce() + Send) {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(64 << 20)
            .spawn_scoped(scope, f)
            .expect("thread")
            .join()
            .expect("passes")
    });
}

#[test]
fn deep_documents_fail_instead_of_overflowing_a_small_stack() {
    on_large_thread(deep_documents);
}

fn deep_documents() {
    let spec = spec(5000);
    assert_eq!(
        on_small_thread(|| validate(&spec, &Options::default())),
        Outcome::Error(STACK_TOO_SMALL.to_owned())
    );
    // Read from YAML, the reader stops as soon as the nesting is too deep.
    let text = format!(
        "openapi: 3.0.3\ninfo: {{title: t, version: '1'}}\npaths: {{}}\nx-deep: {}leaf{}\n",
        "[".repeat(5000),
        "]".repeat(5000)
    );
    assert_eq!(
        on_small_thread(|| load(text.as_bytes(), "<file>")),
        Err(LoadError {
            message: STACK_TOO_SMALL.to_owned()
        })
    );
    drop(spec);
}
