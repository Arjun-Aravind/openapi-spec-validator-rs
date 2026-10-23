//! Python `repr` / `pprint` rendering of JSON values (temporary minimal version).

use std::fmt::Write;

use serde_json::Value;

/// One step of a JSON Schema error path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathItem {
    Key(String),
    Index(usize),
}

pub fn repr(value: &Value) -> String {
    let mut out = String::new();
    write_repr(value, &mut out);
    out
}

fn write_repr(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("None"),
        Value::Bool(true) => out.push_str("True"),
        Value::Bool(false) => out.push_str("False"),
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() => out.push_str(&repr_float(f)),
            _ => out.push_str(&n.to_string()),
        },
        Value::String(s) => out.push_str(&repr_str(s)),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_repr(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (key, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&repr_str(key));
                out.push_str(": ");
                write_repr(item, out);
            }
            out.push('}');
        }
    }
}

pub fn repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

pub fn repr_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_owned();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.to_owned();
    }
    let s = format!("{f:?}");
    if s.contains('e') {
        let (mantissa, exponent) = s.split_once('e').unwrap_or((&s, "0"));
        let (sign, digits) = match exponent.strip_prefix('-') {
            Some(d) => ('-', d),
            None => ('+', exponent),
        };
        let mantissa = mantissa.strip_suffix(".0").unwrap_or(mantissa);
        format!("{mantissa}e{sign}{digits:0>2}")
    } else {
        s
    }
}

pub fn pformat(value: &Value, _width: usize, _sort_dicts: bool) -> String {
    repr(value)
}

pub fn format_as_index(container: &str, indices: &[PathItem]) -> String {
    let mut out = container.to_owned();
    for index in indices {
        match index {
            PathItem::Key(key) => {
                let _ = write!(out, "[{}]", repr_str(key));
            }
            PathItem::Index(i) => {
                let _ = write!(out, "[{i}]");
            }
        }
    }
    out
}

pub fn format_validation_error(
    message: &str,
    validator: &str,
    schema_path: &[PathItem],
    schema: &Value,
    instance_path: &[PathItem],
    instance: &Value,
) -> String {
    format!(
        "{message}\n\nFailed validating {} in {}:\n    {}\n\nOn {}:\n    {}",
        repr_str(validator),
        format_as_index("schema", schema_path),
        pformat(schema, 72, false),
        format_as_index("instance", instance_path),
        pformat(instance, 72, false),
    )
}
