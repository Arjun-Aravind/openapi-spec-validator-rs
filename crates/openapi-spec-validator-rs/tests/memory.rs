//! What validation allocates. A test binary of its own: the allocator below counts every
//! allocation of the process, so nothing else may run alongside.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use openapi_spec_validator_rs::{Options, Outcome, validate};
use serde_json::{Value, json};

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards to the system allocator, only counting the bytes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract is `System.alloc`'s.
        let pointer = unsafe { System.alloc(layout) };
        let current = CURRENT.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        PEAK.fetch_max(current, Ordering::Relaxed);
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: the caller's contract is `System.dealloc`'s.
        unsafe { System.dealloc(pointer, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Invalid 3.0 schemas that nest and repeat: `L0` is 30 `allOf`s around an invalid
/// `type`, and each further level holds ten copies of the one before (as YAML aliases
/// would). Upstream's first error is `L0`'s; the others are never looked at.
#[test]
fn the_first_error_costs_only_itself() {
    let mut level = json!({"type": 5});
    for _ in 0..30 {
        level = json!({"allOf": [level]});
    }
    let mut schemas = serde_json::Map::new();
    schemas.insert("L0".to_owned(), level.clone());
    for name in ["L1", "L2", "L3"] {
        level = json!({"allOf": vec![level; 10]});
        schemas.insert(name.to_owned(), level.clone());
    }
    let spec = json!({
        "openapi": "3.0.3",
        "info": {"title": "t", "version": "1"},
        "paths": {},
        "components": {"schemas": Value::Object(schemas)},
    });

    // The meta-schemas and validators, compiled once per process, are not counted.
    validate(&json!({"openapi": "3.0.3"}), &Options::default());
    PEAK.store(CURRENT.load(Ordering::Relaxed), Ordering::Relaxed);
    let before = CURRENT.load(Ordering::Relaxed);
    let outcome = validate(&spec, &Options::default());
    let used = PEAK.load(Ordering::Relaxed) - before;

    let Outcome::Invalid(errors) = outcome else {
        panic!("invalid: {outcome:?}");
    };
    let details = errors[0].schema_error.as_deref().expect("a schema error");
    assert_eq!(
        details.instance_path,
        ["components", "schemas", "L0"]
            .map(|key| openapi_spec_validator_rs::PathItem::Key(key.to_owned()))
    );
    // Building every error, as 0.2.3 did, takes about 1 GB here.
    assert!(used < 64 << 20, "{} MiB", used >> 20);
}
