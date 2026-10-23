//! OpenAPI format checks as upstream applies them (temporary minimal version).

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatSet {
    Oas30,
    Oas31,
    Oas32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FormatOutcome {
    Unknown,
    Valid,
    Invalid(String),
    #[allow(dead_code)]
    Crash(String),
}

pub fn check_format(
    set: FormatSet,
    format: &str,
    instance: &Value,
    regex_is_valid: &dyn Fn(&str) -> bool,
) -> FormatOutcome {
    let known = matches!(
        format,
        "date"
            | "date-time"
            | "double"
            | "email"
            | "float"
            | "idn-email"
            | "int32"
            | "int64"
            | "ipv4"
            | "ipv6"
            | "password"
            | "regex"
            | "time"
            | "uuid"
    ) || (set == FormatSet::Oas30 && matches!(format, "binary" | "byte"));
    if !known {
        return FormatOutcome::Unknown;
    }
    let ok = match (format, instance) {
        ("regex", Value::String(s)) => regex_is_valid(s),
        ("email" | "idn-email", Value::String(s)) => s.contains('@'),
        ("int32", Value::Number(n)) if !n.is_f64() => {
            n.as_i64().is_some_and(|i| i32::try_from(i).is_ok())
        }
        ("int64", Value::Number(n)) if !n.is_f64() => n.as_i64().is_some(),
        _ => true,
    };
    if ok {
        FormatOutcome::Valid
    } else {
        FormatOutcome::Invalid(format!(
            "{} is not a {}",
            crate::pyrepr::repr(instance),
            crate::pyrepr::repr_str(format)
        ))
    }
}
