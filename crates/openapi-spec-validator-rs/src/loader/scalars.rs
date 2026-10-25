//! `SafeConstructor`'s scalar constructors, with the Python built-ins they rely on.
//!
//! Errors are the `str()` of the Python exception (`ValueError`, `KeyError`, ...), except for
//! `binary`, whose failures PyYAML wraps in a `ConstructorError` (the caller adds the mark).

use super::bigint::BigInt;
use super::pyfmt::{lower, repr_str, repr_str_200, strip, unicode_encode_error};
use crate::pyobj::decimal_to_ascii;

/// `sys.get_int_max_str_digits()` default.
const MAX_STR_DIGITS: usize = 4300;

pub(super) const INT_TO_STR_LIMIT: &str = "Exceeds the limit (4300 digits) for integer string conversion; use sys.set_int_max_str_digits() to increase the limit";

const INDEX_ERROR: &str = "string index out of range";

/// `construct_yaml_bool`.
pub(super) fn bool_value(s: &str) -> Result<bool, String> {
    let key = lower(s);
    match key.as_str() {
        "yes" | "true" | "on" => Ok(true),
        "no" | "false" | "off" => Ok(false),
        // str(KeyError(key)) is the key's repr.
        _ => Err(repr_str(&key)),
    }
}

/// Splits PyYAML's `sign` handling: `value[0]` raises `IndexError` on an empty string.
fn split_sign(value: &str) -> Result<(bool, &str), String> {
    match value.as_bytes().first() {
        None => Err(INDEX_ERROR.to_owned()),
        Some(b'-') => Ok((true, &value[1..])),
        Some(b'+') => Ok((false, &value[1..])),
        Some(_) => Ok((false, value)),
    }
}

/// `construct_yaml_int`.
pub(super) fn int_value(s: &str) -> Result<BigInt, String> {
    let value = s.replace('_', "");
    let (negative, value) = split_sign(&value)?;
    let magnitude = if value == "0" {
        BigInt::default()
    } else if let Some(rest) = value.strip_prefix("0b") {
        py_int(rest, 2)?
    } else if let Some(rest) = value.strip_prefix("0x") {
        py_int(rest, 16)?
    } else if value.starts_with('0') {
        py_int(value, 8)?
    } else if value.is_empty() {
        return Err(INDEX_ERROR.to_owned());
    } else if value.contains(':') {
        // Sum of digit * 60**k; Horner's rule gives the same exact integer.
        let mut acc = BigInt::default();
        for part in value.split(':') {
            acc = acc.mul_small(60).add(&py_int(part, 10)?);
        }
        acc
    } else {
        py_int(value, 10)?
    };
    Ok(if negative {
        magnitude.negated()
    } else {
        magnitude
    })
}

/// Python's `int(s, base)` (PyYAML has already removed underscores).
pub(super) fn py_int(s: &str, base: u32) -> Result<BigInt, String> {
    let invalid = || {
        format!(
            "invalid literal for int() with base {base}: {}",
            repr_str_200(s)
        )
    };
    let ascii = decimal_to_ascii(s).ok_or_else(invalid)?;
    let t = strip(&ascii);
    let (negative, t) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let prefix = match base {
        16 => ["0x", "0X"],
        8 => ["0o", "0O"],
        2 => ["0b", "0B"],
        _ => ["", ""],
    };
    let t = prefix
        .iter()
        .filter(|p| !p.is_empty())
        .find_map(|p| t.strip_prefix(p))
        .unwrap_or(t);
    let run = t
        .bytes()
        .take_while(|&b| char::from(b).is_digit(base))
        .count();
    // CPython checks the digit limit before looking at what follows the digits.
    if !base.is_power_of_two() && run > MAX_STR_DIGITS {
        return Err(format!(
            "Exceeds the limit ({MAX_STR_DIGITS} digits) for integer string conversion: value has {run} digits; use sys.set_int_max_str_digits() to increase the limit"
        ));
    }
    if run == 0 || run != t.len() {
        return Err(invalid());
    }
    let n = BigInt::from_digits(t.as_bytes(), base);
    Ok(if negative { n.negated() } else { n })
}

/// The float a scalar constructs to. PyYAML's `.nan` is one shared object (`nan_value`);
/// every other NaN is a fresh object, which matters for dict-key identity.
pub(super) enum FloatValue {
    Num(f64),
    SharedNan,
}

/// `construct_yaml_float`.
pub(super) fn float_value(s: &str) -> Result<FloatValue, String> {
    let value = lower(&s.replace('_', ""));
    let (negative, value) = split_sign(&value)?;
    let sign = if negative { -1.0 } else { 1.0 };
    if value == ".inf" {
        return Ok(FloatValue::Num(sign * f64::INFINITY));
    }
    if value == ".nan" {
        return Ok(FloatValue::SharedNan);
    }
    if value.contains(':') {
        let digits = value
            .split(':')
            .map(py_float)
            .collect::<Result<Vec<f64>, String>>()?;
        let mut acc = 0.0;
        let mut base = BigInt::from_i128(1);
        for (k, digit) in digits.iter().rev().enumerate() {
            // `digit * base` converts the int base to float first (exactly, for small k).
            let base_f = if k < 21 {
                base.to_i128().map_or(f64::INFINITY, |b| b as f64)
            } else {
                base.to_decimal().parse().unwrap_or(f64::INFINITY)
            };
            acc += digit * base_f;
            base = base.mul_small(60);
        }
        return Ok(FloatValue::Num(sign * acc));
    }
    Ok(FloatValue::Num(sign * py_float(value)?))
}

/// Python's `float(s)`.
pub(super) fn py_float(s: &str) -> Result<f64, String> {
    let invalid = || format!("could not convert string to float: {}", repr_str(s));
    let ascii = decimal_to_ascii(s).ok_or_else(invalid)?;
    let t = strip(&ascii);
    // Rust's grammar matches Python's for these strings: optional sign, digits with an optional
    // point and exponent, or inf/infinity/nan in any case. Underscores are already gone.
    let plausible = !t.is_empty()
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'));
    plausible
        .then(|| t.parse::<f64>().ok())
        .flatten()
        .ok_or_else(invalid)
}

/// `construct_yaml_binary`'s checks; the bytes themselves are never needed because
/// `json.dumps` rejects them.
pub(super) fn binary_problem(s: &str) -> Option<String> {
    if let Some(start) = s.chars().position(|c| !c.is_ascii()) {
        let chars: Vec<char> = s.chars().collect();
        let end = start + chars[start..].iter().take_while(|c| !c.is_ascii()).count();
        return Some(format!(
            "failed to convert base64 data into ascii: {}",
            unicode_encode_error("ascii", &chars, start, end, "ordinal not in range(128)")
        ));
    }
    a2b_base64(s.as_bytes())
        .err()
        .map(|e| format!("failed to decode base64 data: {e}"))
}

/// `binascii.a2b_base64(data, strict_mode=False)`, returning only the decoded length.
fn a2b_base64(data: &[u8]) -> Result<usize, String> {
    let mut quad_pos = 0;
    let mut pads = 0;
    let mut written = 0usize;
    for &c in data {
        if c == b'=' {
            if quad_pos >= 2 {
                pads += 1;
                if quad_pos + pads >= 4 {
                    return Ok(written);
                }
            }
            continue;
        }
        if !(c.is_ascii_alphanumeric() || c == b'+' || c == b'/') {
            continue;
        }
        pads = 0;
        if quad_pos != 0 {
            written += 1;
        }
        quad_pos = (quad_pos + 1) % 4;
    }
    match quad_pos {
        0 => Ok(written),
        1 => Err(format!(
            "Invalid base64-encoded string: number of data characters ({}) cannot be 1 more than a multiple of 4",
            written / 3 * 4 + 1
        )),
        _ => Err("Incorrect padding".to_owned()),
    }
}

pub(super) enum Timestamp {
    Date,
    DateTime,
}

struct TsParts<'a> {
    year: &'a str,
    month: &'a str,
    day: &'a str,
    time: Option<TimeParts<'a>>,
}

struct TimeParts<'a> {
    hour: &'a str,
    minute: &'a str,
    second: &'a str,
    tz: Option<Tz<'a>>,
}

enum Tz<'a> {
    Utc,
    Offset {
        negative: bool,
        hour: &'a str,
        minute: Option<&'a str>,
    },
}

/// `construct_yaml_timestamp` (only reachable through an explicit `!!timestamp` tag).
pub(super) fn timestamp_value(s: &str) -> Result<Timestamp, String> {
    let parts = match_timestamp(s.strip_suffix('\n').unwrap_or(s))
        .ok_or_else(|| "'NoneType' object has no attribute 'groupdict'".to_owned())?;
    let num = |v: &str| v.parse::<i64>().unwrap_or(0);
    let (year, month, day) = (num(parts.year), num(parts.month), num(parts.day));
    let Some(time) = parts.time else {
        check_date(year, month, day)?;
        return Ok(Timestamp::Date);
    };
    if let Some(Tz::Offset {
        negative,
        hour,
        minute,
    }) = time.tz
    {
        let mut seconds = num(hour) * 3600 + minute.map_or(0, num) * 60;
        if negative {
            seconds = -seconds;
        }
        if seconds.abs() >= 86_400 {
            return Err(format!(
                "offset must be a timedelta strictly between -timedelta(hours=24) and timedelta(hours=24), not {}.",
                timedelta_repr(seconds)
            ));
        }
    }
    check_date(year, month, day)?;
    for (value, name, max) in [
        (num(time.hour), "hour", 23),
        (num(time.minute), "minute", 59),
        (num(time.second), "second", 59),
    ] {
        if value > max {
            return Err(format!("{name} must be in 0..{max}"));
        }
    }
    Ok(Timestamp::DateTime)
}

fn check_date(year: i64, month: i64, day: i64) -> Result<(), String> {
    if !(1..=9999).contains(&year) {
        return Err(format!("year {year} is out of range"));
    }
    if !(1..=12).contains(&month) {
        return Err("month must be in 1..12".to_owned());
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=days).contains(&day) {
        return Err("day is out of range for month".to_owned());
    }
    Ok(())
}

/// `repr(datetime.timedelta(seconds=s))`.
fn timedelta_repr(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let secs = seconds.rem_euclid(86_400);
    match (days, secs) {
        (0, 0) => "datetime.timedelta(0)".to_owned(),
        (0, s) => format!("datetime.timedelta(seconds={s})"),
        (d, 0) => format!("datetime.timedelta(days={d})"),
        (d, s) => format!("datetime.timedelta(days={d}, seconds={s})"),
    }
}

/// Splits `s` into a run of 1..=max ASCII digits, trying the longest run first (regex
/// backtracking order), and calls `rest` on what follows until it succeeds.
fn digits_then<'a, T>(
    s: &'a str,
    min: usize,
    max: usize,
    rest: impl Fn(&'a str, &'a str) -> Option<T>,
) -> Option<T> {
    let avail = s.bytes().take_while(u8::is_ascii_digit).count().min(max);
    (min..=avail).rev().find_map(|n| rest(&s[..n], &s[n..]))
}

/// `SafeConstructor.timestamp_regexp` (anchored at both ends).
fn match_timestamp(s: &str) -> Option<TsParts<'_>> {
    digits_then(s, 4, 4, |year, rest| {
        let rest = rest.strip_prefix('-')?;
        digits_then(rest, 1, 2, |month, rest| {
            let rest = rest.strip_prefix('-')?;
            digits_then(rest, 1, 2, |day, rest| {
                if rest.is_empty() {
                    return Some(TsParts {
                        year,
                        month,
                        day,
                        time: None,
                    });
                }
                let time = match_time(rest)?;
                Some(TsParts {
                    year,
                    month,
                    day,
                    time: Some(time),
                })
            })
        })
    })
}

fn match_time(s: &str) -> Option<TimeParts<'_>> {
    let rest = if let Some(r) = s.strip_prefix(['T', 't']) {
        r
    } else {
        let r = s.trim_start_matches([' ', '\t']);
        if r.len() == s.len() {
            return None;
        }
        r
    };
    digits_then(rest, 1, 2, |hour, rest| {
        let rest = rest.strip_prefix(':')?;
        digits_then(rest, 2, 2, |minute, rest| {
            let rest = rest.strip_prefix(':')?;
            digits_then(rest, 2, 2, |second, rest| {
                let rest = match rest.strip_prefix('.') {
                    Some(r) => r.trim_start_matches(|c: char| c.is_ascii_digit()),
                    None => rest,
                };
                if rest.is_empty() {
                    return Some(TimeParts {
                        hour,
                        minute,
                        second,
                        tz: None,
                    });
                }
                let tz = match_tz(rest.trim_start_matches([' ', '\t']))?;
                Some(TimeParts {
                    hour,
                    minute,
                    second,
                    tz: Some(tz),
                })
            })
        })
    })
}

fn match_tz(s: &str) -> Option<Tz<'_>> {
    if s == "Z" {
        return Some(Tz::Utc);
    }
    let negative = s.starts_with('-');
    let rest = s.strip_prefix(['-', '+'])?;
    digits_then(rest, 1, 2, |hour, rest| {
        if rest.is_empty() {
            return Some(Tz::Offset {
                negative,
                hour,
                minute: None,
            });
        }
        let minute = rest.strip_prefix(':')?;
        (minute.len() == 2 && minute.bytes().all(|b| b.is_ascii_digit())).then_some(Tz::Offset {
            negative,
            hour,
            minute: Some(minute),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_semantics() {
        let int = |s: &str| int_value(s).map(|n| n.to_decimal());
        assert_eq!(int("0b1010_0111").as_deref(), Ok("167"));
        assert_eq!(int("-1:-30").as_deref(), Ok("-30"));
        assert_eq!(int("0x0x1f").as_deref(), Ok("31"));
        assert_eq!(int(" 12 ").as_deref(), Ok("12"));
        assert_eq!(int("").unwrap_err(), "string index out of range");
        assert_eq!(int("-").unwrap_err(), "string index out of range");
        assert_eq!(
            int("0b").unwrap_err(),
            "invalid literal for int() with base 2: ''"
        );
        assert_eq!(
            int("08").unwrap_err(),
            "invalid literal for int() with base 8: '08'"
        );
    }

    #[test]
    fn base64_errors() {
        assert_eq!(a2b_base64(b"aGk="), Ok(2));
        assert_eq!(a2b_base64(b"aGk").unwrap_err(), "Incorrect padding");
        assert!(
            a2b_base64(b"a")
                .unwrap_err()
                .contains("(1) cannot be 1 more")
        );
        assert_eq!(a2b_base64(b"aGVsbG8="), Ok(5));
    }

    #[test]
    fn timestamps() {
        assert!(matches!(timestamp_value("2020-01-01"), Ok(Timestamp::Date)));
        assert!(matches!(
            timestamp_value("2001-12-14 21:59:43.10 -5"),
            Ok(Timestamp::DateTime)
        ));
        assert_eq!(
            timestamp_value("2020-01-01 10:00:00 -24:30")
                .err()
                .as_deref(),
            Some(
                "offset must be a timedelta strictly between -timedelta(hours=24) and timedelta(hours=24), not datetime.timedelta(days=-2, seconds=84600)."
            )
        );
        assert!(timestamp_value("2020-1-011").is_err());
    }
}
