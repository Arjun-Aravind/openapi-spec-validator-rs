//! Spec version detection, mirroring upstream's `SpecVersionFinder`.

use std::fmt;

use serde_json::Value;

use crate::pyrepr;

/// The specification versions upstream can validate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpecVersion {
    /// Swagger 2.0 (`swagger: "2.0"`).
    V2_0,
    /// OpenAPI 3.0.x.
    V3_0,
    /// OpenAPI 3.1.x.
    V3_1,
    /// OpenAPI 3.2.x.
    V3_2,
}

impl SpecVersion {
    /// Versions in the order upstream tries them.
    pub const ALL: [SpecVersion; 4] = [Self::V2_0, Self::V3_0, Self::V3_1, Self::V3_2];

    /// The root keyword that carries the version string.
    pub fn keyword(self) -> &'static str {
        match self {
            Self::V2_0 => "swagger",
            Self::V3_0 | Self::V3_1 | Self::V3_2 => "openapi",
        }
    }

    fn major_minor(self) -> (&'static str, &'static str) {
        match self {
            Self::V2_0 => ("2", "0"),
            Self::V3_0 => ("3", "0"),
            Self::V3_1 => ("3", "1"),
            Self::V3_2 => ("3", "2"),
        }
    }

    /// Parses a `--schema` CLI value (`2.0`, `3.0`, `3.0.0`, ...).
    pub fn from_cli(value: &str) -> Option<Self> {
        match value {
            "2.0" => Some(Self::V2_0),
            "3.0" | "3.0.0" => Some(Self::V3_0),
            "3.1" | "3.1.0" => Some(Self::V3_1),
            "3.2" | "3.2.0" => Some(Self::V3_2),
            _ => None,
        }
    }
}

impl fmt::Display for SpecVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (major, minor) = self.major_minor();
        write!(f, "{major}.{minor}")
    }
}

/// Why no version could be detected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DetectError {
    /// No known version keyword/value: upstream raises `ValidatorDetectError()`,
    /// whose message is empty.
    NotFound,
    /// Upstream crashes with a `TypeError` (e.g. `swagger: 2.0` parsed as a float);
    /// carries the Python exception message.
    TypeError(String),
}

impl fmt::Display for DetectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => Ok(()),
            Self::TypeError(message) => f.write_str(message),
        }
    }
}

/// Detects the spec version the way upstream does.
///
/// Upstream checks `keyword in spec`, then matches `(\d+)\.(\d+)(\..*)?` at the
/// start of the value and compares major/minor as strings. Non-mapping documents
/// follow Python's `in` / `[]` semantics, which is why some of them produce a
/// `TypeError` rather than "not found".
pub fn detect_version(spec: &Value) -> Result<SpecVersion, DetectError> {
    for version in SpecVersion::ALL {
        let keyword = version.keyword();
        let version_value = match spec {
            Value::Object(map) => match map.get(keyword) {
                Some(value) => value,
                None => continue,
            },
            Value::String(s) => {
                if s.contains(keyword) {
                    return Err(DetectError::TypeError(
                        "string indices must be integers, not 'str'".to_owned(),
                    ));
                }
                continue;
            }
            Value::Array(items) => {
                if items.iter().any(|item| item.as_str() == Some(keyword)) {
                    return Err(DetectError::TypeError(
                        "list indices must be integers or slices, not str".to_owned(),
                    ));
                }
                continue;
            }
            other => {
                return Err(DetectError::TypeError(format!(
                    "argument of type '{}' is not iterable",
                    python_type_name(other)
                )));
            }
        };
        let Value::String(version_str) = version_value else {
            return Err(DetectError::TypeError(format!(
                "expected string or bytes-like object, got '{}'",
                python_type_name(version_value)
            )));
        };
        if parse_major_minor(version_str) == Some(version.major_minor()) {
            return Ok(version);
        }
    }
    Err(DetectError::NotFound)
}

/// Extracts the `major` and `minor` groups of `(\d+)\.(\d+)` anchored at the start.
///
/// Python's `\d` also matches non-ASCII decimal digits; such versions can never
/// equal the expected ASCII values, so treating them as non-digits only differs for
/// inputs like `3.0` followed by another script's digit.
fn parse_major_minor(s: &str) -> Option<(&str, &str)> {
    let major_len = s.bytes().take_while(u8::is_ascii_digit).count();
    if major_len == 0 || s.as_bytes().get(major_len) != Some(&b'.') {
        return None;
    }
    let rest = &s[major_len + 1..];
    let minor_len = rest.bytes().take_while(u8::is_ascii_digit).count();
    if minor_len == 0 {
        return None;
    }
    Some((&s[..major_len], &rest[..minor_len]))
}

/// The Python type name of a loaded value as C-level exception messages print it
/// (`tp_name`; module-qualified for an object of a static type outside `builtins`, see
/// [`pyrepr::Foreign::type_name`]).
pub(crate) fn python_type_name(value: &Value) -> &str {
    if let Some(object) = pyrepr::foreign(value) {
        return object.type_name;
    }
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn detects_known_versions() {
        assert_eq!(
            detect_version(&json!({"swagger": "2.0"})),
            Ok(SpecVersion::V2_0)
        );
        assert_eq!(
            detect_version(&json!({"openapi": "3.0.3"})),
            Ok(SpecVersion::V3_0)
        );
        assert_eq!(
            detect_version(&json!({"openapi": "3.1.0"})),
            Ok(SpecVersion::V3_1)
        );
        assert_eq!(
            detect_version(&json!({"openapi": "3.2.0"})),
            Ok(SpecVersion::V3_2)
        );
        // `match` is anchored only at the start, so suffixes are ignored.
        assert_eq!(
            detect_version(&json!({"openapi": "3.0-rc"})),
            Ok(SpecVersion::V3_0)
        );
        assert_eq!(
            detect_version(&json!({"openapi": "3.1.x.y"})),
            Ok(SpecVersion::V3_1)
        );
    }

    #[test]
    fn unknown_versions_are_not_found() {
        for spec in [
            json!({"openapi": "3.00.1"}),
            json!({"openapi": "4.0.0"}),
            json!({"openapi": "v3.0"}),
            json!({"swagger": "1.2"}),
            json!({"info": {}}),
            json!([]),
            json!("plain text"),
        ] {
            assert_eq!(detect_version(&spec), Err(DetectError::NotFound), "{spec}");
        }
    }

    #[test]
    fn non_string_versions_raise_type_errors() {
        let err = detect_version(&json!({"swagger": 2.0})).unwrap_err();
        assert_eq!(
            err.to_string(),
            "expected string or bytes-like object, got 'float'"
        );
        let err = detect_version(&Value::Null).unwrap_err();
        assert_eq!(
            err.to_string(),
            "argument of type 'NoneType' is not iterable"
        );
        let err = detect_version(&json!("swagger: 2.0")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "string indices must be integers, not 'str'"
        );
    }

    #[test]
    fn swagger_keyword_wins_over_openapi() {
        // Upstream tries swagger first and stops at the first keyword it can't parse.
        let err = detect_version(&json!({"swagger": 2, "openapi": "3.0.0"})).unwrap_err();
        assert_eq!(
            err.to_string(),
            "expected string or bytes-like object, got 'int'"
        );
    }
}
