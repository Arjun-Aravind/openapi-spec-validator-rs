//! Prints the frame model's prediction for documents, for `scripts/frames/compare.py`.
//!
//! Reads JSON lines `{"file": PATH, "at": [POINTER, ...]}` on standard input and prints
//! `{"file": PATH, "doc": N, "meta": {POINTER: N}}` for each.

use std::io::BufRead;

use openapi_spec_validator_rs::{detect_version, load, predicted_frames};

fn main() {
    for line in std::io::stdin().lock().lines() {
        let request: serde_json::Value = serde_json::from_str(&line.expect("stdin")).expect("json");
        let file = request["file"].as_str().expect("file");
        let bytes = std::fs::read(file).expect("readable");
        // Documents that do not load or name no known version have no meta-schema check.
        let Ok(spec) = load(&bytes, "<urllib response>") else {
            println!(
                "{}",
                serde_json::json!({"file": file, "doc": null, "meta": {}})
            );
            continue;
        };
        let Ok(version) = detect_version(&spec) else {
            println!(
                "{}",
                serde_json::json!({"file": file, "doc": null, "meta": {}})
            );
            continue;
        };
        // The document check comes first, as upstream runs it: the model's regular
        // expression cache warms in that order.
        let doc = predicted_frames(&spec, version, None);
        let mut meta = serde_json::Map::new();
        for at in request["at"].as_array().into_iter().flatten() {
            let at = at.as_str().expect("pointer");
            meta.insert(
                at.to_owned(),
                predicted_frames(&spec, version, Some(at)).into(),
            );
        }
        println!(
            "{}",
            serde_json::json!({"file": file, "doc": doc, "meta": meta})
        );
    }
}
