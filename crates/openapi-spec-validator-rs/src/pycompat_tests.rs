//! Differential tests against fixtures generated from CPython 3.13, jsonschema 4.26 and
//! openapi-schema-validator 0.9.0 by `scripts/gen_pycompat_fixtures.py` (a sample is checked in).

use crate::formats::{FormatOutcome, FormatSet, check_format, check_format_with_cause};
use crate::pyformat::formatter_field_names;
use crate::pyrepr::{
    ErrorKind, PathItem, format_as_index, format_jsonschema_error, format_validation_error,
    pformat, repr, repr_float, repr_str,
};
use serde_json::Value;
use std::path::Path;

fn fixture(name: &str) -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pycompat")
        .join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    text.lines()
        .map(|line| serde_json::from_str(line).expect("fixture line is JSON"))
        .collect()
}

/// Runs `check` on every case; reports agreement and the first few disagreements.
fn run(name: &str, check: impl Fn(&Value) -> Result<(), String>) {
    let cases = fixture(name);
    let mut failures = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        if let Err(msg) = check(case) {
            failures.push(format!("#{i}: {msg}"));
        }
    }
    let total = cases.len();
    let agree = total - failures.len();
    eprintln!("{name}: {agree}/{total} agree");
    if !failures.is_empty() {
        for f in failures.iter().take(8) {
            eprintln!("---\n{f}");
        }
        panic!("{name}: {} of {total} cases disagree", failures.len());
    }
}

fn expect_eq(got: &str, want: &str, context: impl FnOnce() -> String) -> Result<(), String> {
    if got == want {
        Ok(())
    } else {
        Err(format!("{}\n  want: {want:?}\n  got:  {got:?}", context()))
    }
}

fn s(v: &Value) -> &str {
    v.as_str().expect("string field")
}

fn path_items(v: &Value) -> Vec<PathItem> {
    v.as_array()
        .expect("path array")
        .iter()
        .map(|item| match (item.get("key"), item.get("index")) {
            (Some(k), _) => PathItem::Key(s(k).to_owned()),
            (_, Some(i)) => PathItem::Index(i.as_u64().expect("index") as usize),
            _ => panic!("bad path item {item}"),
        })
        .collect()
}

#[test]
fn repr_values() {
    run("repr.jsonl", |c| {
        expect_eq(&repr(&c["value"]), s(&c["repr"]), || c["value"].to_string())
    });
}

#[test]
fn repr_strings() {
    run("repr_str.jsonl", |c| {
        expect_eq(&repr_str(s(&c["s"])), s(&c["repr"]), || c["s"].to_string())
    });
}

#[test]
fn repr_floats() {
    run("repr_float.jsonl", |c| {
        let bits = u64::from_str_radix(s(&c["bits"]), 16).expect("hex bits");
        expect_eq(&repr_float(f64::from_bits(bits)), s(&c["repr"]), || {
            format!("bits {bits:016x}")
        })
    });
}

fn pformat_case(c: &Value) -> Result<(), String> {
    let width = c["width"].as_u64().expect("width") as usize;
    let sort = c["sort_dicts"].as_bool().expect("sort_dicts");
    let got = pformat(&c["value"], width, sort);
    expect_eq(&got, s(&c["out"]), || {
        format!("width={width} sort_dicts={sort} value={}", c["value"])
    })
}

#[test]
fn pformat_random() {
    run("pformat.jsonl", pformat_case);
}

#[test]
fn pformat_spec_subtrees() {
    run("pformat_specs.jsonl", pformat_case);
}

#[test]
fn format_as_index_cases() {
    run("format_as_index.jsonl", |c| {
        expect_eq(
            &format_as_index(s(&c["container"]), &path_items(&c["indices"])),
            s(&c["out"]),
            || c.to_string(),
        )
    });
}

#[test]
fn validation_error_str() {
    run("validation_error.jsonl", |c| {
        let kind = if s(&c["kind"]) == "schema" {
            ErrorKind::Schema
        } else {
            ErrorKind::Validation
        };
        let schema_path = path_items(&c["schema_path"]);
        let instance_path = path_items(&c["instance_path"]);
        let got = format_jsonschema_error(
            kind,
            s(&c["message"]),
            c["validator"].as_str(),
            &schema_path,
            &c["schema"],
            &instance_path,
            &c["instance"],
        );
        if let (ErrorKind::Validation, Some(validator)) = (kind, c["validator"].as_str()) {
            let simple = format_validation_error(
                s(&c["message"]),
                validator,
                &schema_path,
                &c["schema"],
                &instance_path,
                &c["instance"],
            );
            assert_eq!(simple, got);
        }
        expect_eq(&got, s(&c["str"]), || {
            format!("message={:?} validator={}", c["message"], c["validator"])
        })
    });
}

#[test]
fn formatter_parse() {
    run("formatter.jsonl", |c| {
        let got = formatter_field_names(s(&c["s"]));
        let want: Result<Vec<Option<String>>, String> = match c["error"].as_str() {
            Some(e) => Err(e.to_owned()),
            None => Ok(c["names"]
                .as_array()
                .expect("names")
                .iter()
                .map(|n| n.as_str().map(str::to_owned))
                .collect()),
        };
        if got == want {
            Ok(())
        } else {
            Err(format!(
                "{:?}\n  want: {want:?}\n  got:  {got:?}",
                s(&c["s"])
            ))
        }
    });
}

#[test]
fn format_checkers() {
    let mut per_format: std::collections::BTreeMap<String, usize> = Default::default();
    run("formats.jsonl", |c| {
        let set = match s(&c["set"]) {
            "oas30" => FormatSet::Oas30,
            "oas31" => FormatSet::Oas31,
            _ => FormatSet::Oas32,
        };
        let format = s(&c["format"]);
        // The fixture records Python's re.compile verdict for string instances.
        let regex = |p: &str| {
            assert_eq!(Some(p), c["instance"].as_str());
            match &c["regex_valid"] {
                Value::Bool(valid) => Ok(*valid),
                other => Err(s(other).to_owned()),
            }
        };
        let (outcome, cause) = check_format_with_cause(set, format, &c["instance"], &regex);
        if c["regex_valid"].is_boolean() || format != "regex" {
            let simple = |p: &str| regex(p).unwrap_or(false);
            assert_eq!(outcome, check_format(set, format, &c["instance"], &simple));
        }
        let message = c["message"].as_str().map(str::to_owned);
        let want = match s(&c["outcome"]) {
            "unknown" => FormatOutcome::Unknown,
            "valid" => FormatOutcome::Valid,
            "invalid" => FormatOutcome::Invalid(message.clone().unwrap_or_default()),
            _ => FormatOutcome::Crash(message.clone().unwrap_or_default()),
        };
        let want_cause = c["cause"].as_str().map(str::to_owned);
        if outcome == want && cause == want_cause {
            Ok(())
        } else {
            Err(format!(
                "{} {format:?} {}\n  want: {want:?} cause={want_cause:?}\n  got:  {outcome:?} cause={cause:?}",
                s(&c["set"]),
                c["instance"]
            ))
        }
    });
    for c in fixture("formats.jsonl") {
        *per_format
            .entry(format!("{}/{}", s(&c["set"]), s(&c["format"])))
            .or_default() += 1;
    }
    let min = per_format.values().min().copied().unwrap_or(0);
    eprintln!(
        "formats: {} (set, format) pairs, min {min} values each",
        per_format.len()
    );
}

#[test]
fn repr_floats_bulk() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pycompat/repr_float_bulk.txt");
    let text = std::fs::read_to_string(path).expect("bulk float fixture");
    let mut failures = Vec::new();
    let mut total = 0;
    for line in text.lines() {
        let (bits, want) = line.split_once(' ').expect("bits repr");
        let bits = u64::from_str_radix(bits, 16).expect("hex bits");
        total += 1;
        let got = repr_float(f64::from_bits(bits));
        if got != want {
            failures.push(format!("{bits:016x}: want {want} got {got}"));
        }
    }
    eprintln!(
        "repr_float_bulk.txt: {}/{total} agree",
        total - failures.len()
    );
    assert!(
        failures.is_empty(),
        "{:#?}",
        &failures[..failures.len().min(10)]
    );
}
