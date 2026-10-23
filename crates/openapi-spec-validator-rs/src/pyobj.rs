//! Python object semantics for JSON-loaded values (equality, truthiness, `str()`).

use std::cmp::Ordering;

use serde_json::{Number, Value};

use crate::pyrepr;
use crate::version::python_type_name;

/// Python truthiness.
pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// `str(value)`: strings print as-is, everything else as its repr.
pub(crate) fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => pyrepr::repr(other),
    }
}

/// `repr(type(value))`, e.g. `<class 'int'>`.
pub(crate) fn type_class(value: &Value) -> String {
    format!("<class '{}'>", python_type_name(value))
}

/// Python `==` between JSON-loaded objects (`True == 1`, `1 == 1.0`).
pub(crate) fn py_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| py_eq(a, b))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(key, a)| y.get(key).is_some_and(|b| py_eq(a, b)))
        }
        _ => match (as_number(a), as_number(b)) {
            (Some(x), Some(y)) => num_cmp(&x, &y) == Some(Ordering::Equal),
            _ => false,
        },
    }
}

/// jsonschema's `_utils.equal`: like `==`, but booleans never equal numbers.
pub(crate) fn json_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Bool(_), _) | (_, Value::Bool(_)) => false,
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| json_equal(a, b))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(key, a)| y.get(key).is_some_and(|b| json_equal(a, b)))
        }
        _ => py_eq(a, b),
    }
}

/// A number with Python's int/float distinction (bools count as ints).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Num {
    Int(i128),
    Float(f64),
}

pub(crate) fn as_number(value: &Value) -> Option<Num> {
    match value {
        Value::Bool(b) => Some(Num::Int(i128::from(*b))),
        Value::Number(n) => Some(number(n)),
        _ => None,
    }
}

pub(crate) fn number(n: &Number) -> Num {
    if let Some(i) = n.as_i64() {
        Num::Int(i128::from(i))
    } else if let Some(u) = n.as_u64() {
        Num::Int(i128::from(u))
    } else {
        Num::Float(n.as_f64().unwrap_or(f64::NAN))
    }
}

impl Num {
    pub(crate) fn as_f64(self) -> f64 {
        match self {
            // Precision loss only above 2^53, where Python compares exactly.
            #[allow(clippy::cast_precision_loss)]
            Self::Int(i) => i as f64,
            Self::Float(f) => f,
        }
    }
}

/// Python numeric comparison (`None` for NaN).
pub(crate) fn num_cmp(a: &Num, b: &Num) -> Option<Ordering> {
    match (a, b) {
        (Num::Int(x), Num::Int(y)) => Some(x.cmp(y)),
        _ => a.as_f64().partial_cmp(&b.as_f64()),
    }
}

/// `hash(value)` succeeds: lists and dicts are unhashable.
pub(crate) fn check_hashable(value: &Value) -> Result<(), String> {
    match value {
        Value::Array(_) | Value::Object(_) => {
            Err(format!("unhashable type: '{}'", python_type_name(value)))
        }
        _ => Ok(()),
    }
}

/// The elements of `iter(value)`: list items, dict keys, or string characters.
pub(crate) fn iterate(value: &Value) -> Result<Vec<Value>, String> {
    match value {
        Value::Array(items) => Ok(items.clone()),
        Value::Object(map) => Ok(map.keys().map(|k| Value::String(k.clone())).collect()),
        Value::String(s) => Ok(s.chars().map(|c| Value::String(c.to_string())).collect()),
        other => Err(format!(
            "'{}' object is not iterable",
            python_type_name(other)
        )),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn equality_follows_python() {
        assert!(py_eq(&json!(1), &json!(1.0)));
        assert!(py_eq(&json!(true), &json!(1)));
        assert!(!json_equal(&json!(true), &json!(1)));
        assert!(json_equal(&json!([1, {"a": 2.0}]), &json!([1.0, {"a": 2}])));
        assert!(!py_eq(&json!("1"), &json!(1)));
    }

    #[test]
    fn truthiness() {
        assert!(!truthy(&json!(0.0)));
        assert!(!truthy(&json!([])));
        assert!(truthy(&json!("x")));
    }
}
