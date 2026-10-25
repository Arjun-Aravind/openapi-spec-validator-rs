//! A cancelled validation stops at its next check, on every thread it uses.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use openapi_spec_validator_rs::{Cancelled, Options, run_cancellable, validate};
use serde_json::json;

#[test]
fn cancelled_validation_unwinds_with_its_payload() {
    // Big enough for the two passes to run on threads of their own.
    let schemas: serde_json::Map<String, serde_json::Value> = (0..2000)
        .map(|i| {
            (
                format!("S{i}"),
                json!({"type": "object", "properties": {"a": {"type": "string"}}}),
            )
        })
        .collect();
    let spec = json!({
        "openapi": "3.0.3",
        "info": {"title": "t", "version": "1"},
        "paths": {},
        "components": {"schemas": schemas},
    });
    let flag = Arc::new(AtomicBool::new(false));
    let outcome = run_cancellable(Arc::clone(&flag), || validate(&spec, &Options::default()));
    assert_eq!(outcome, openapi_spec_validator_rs::Outcome::Valid);

    flag.store(true, Ordering::Relaxed);
    let unwound =
        std::panic::catch_unwind(|| run_cancellable(flag, || validate(&spec, &Options::default())));
    let payload = unwound.expect_err("a cancelled validation does not return");
    assert!(payload.is::<Cancelled>());
}
