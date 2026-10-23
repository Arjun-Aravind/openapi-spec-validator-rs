//! OpenAPI `format` checks exactly as upstream applies them: openapi-schema-validator 0.9.0's
//! `oas30_format_checker` / `oas31_format_checker` / `oas32_format_checker`, which are
//! jsonschema 4.26 `FormatChecker`s seeded from jsonschema's class-level registry (the checks
//! whose optional libraries are installed) plus openapi-schema-validator's own checks.
//!
//! Effective registries (from `sorted(checker.checkers)`):
//! * oas30: binary, byte, date, date-time, double, email, float, idn-email, int32, int64, ipv4,
//!   ipv6, password, regex, time, uuid
//! * oas31 / oas32: the same without binary and byte.
//!
//! Note `time` is jsonschema's *draft 3* checker (`datetime.strptime(s, "%H:%M:%S")`): it is
//! registered last on the shared class registry, so it wins over the RFC 3339 one.
//!
//! A `Crash` escapes jsonschema and the upstream CLI prints it as `<file>: Error: <message>`
//! (e.g. `format: byte` with a non-ASCII value in OAS 3.0).
//!
//! Depends on the sibling `pyrepr` module for Python `repr()`.

use super::pyrepr::{PyNumber, py_isspace, py_number, repr, repr_str};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatSet {
    Oas30,
    Oas31,
    Oas32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FormatOutcome {
    /// format not in the set: no check (annotation only)
    Unknown,
    Valid,
    /// `FormatChecker.check` raised `FormatError`; carries its message,
    /// `f"{instance!r} is not a {format!r}"`, e.g. `'x' is not a 'date'`.
    Invalid(String),
    /// The check raised an exception NOT listed in its `raises`, which propagates out of
    /// validation in Python; carries `str(exc)`.
    Crash(String),
}

/// Equivalent of `checker.check(instance, format)` for one format.
///
/// `regex_is_valid` must implement Python `re.compile` validity (used for `regex`).
#[cfg(test)]
pub fn check_format(
    set: FormatSet,
    format: &str,
    instance: &Value,
    regex_is_valid: &dyn Fn(&str) -> bool,
) -> FormatOutcome {
    check_format_with_cause(set, format, instance, &|s| Ok(regex_is_valid(s))).0
}

/// Like [`check_format`], also returning `str(error.cause)` for an `Invalid` outcome whose
/// check raised one of its listed `raises` exceptions. jsonschema copies that exception to
/// `ValidationError.cause`, which openapi-spec-validator's CLI prints under `# Cause`.
///
/// `regex_check` is `re.compile`: `Ok(valid)`, where `re.error` means `Ok(false)`, or
/// `Err(str(exc))` for the other exceptions it can raise, which openapi-schema-validator's
/// `is_regex` does not catch (`OverflowError: the repetition number is too large` for
/// `{n}` with n >= 2**32 - 1, `RecursionError` for very deep group nesting).
pub fn check_format_with_cause(
    set: FormatSet,
    format: &str,
    instance: &Value,
    regex_check: &dyn Fn(&str) -> Result<bool, String>,
) -> (FormatOutcome, Option<String>) {
    let oas30 = set == FormatSet::Oas30;
    let result = match format {
        "int32" => Check::from_bool(is_int(instance, 31)),
        "int64" => Check::from_bool(is_int(instance, 63)),
        // `is_float` / `is_double`: ints and bools pass, non-numbers pass, floats pass.
        "float" | "double" | "password" => Check::Pass,
        "binary" if oas30 => Check::Pass, // is_binary_pragmatic
        "byte" if oas30 => with_str(instance, is_byte),
        "regex" => with_str(instance, |s| match regex_check(s) {
            Ok(valid) => Check::from_bool(valid),
            Err(exc) => Check::Crash(exc),
        }),
        "email" | "idn-email" => with_str(instance, |s| Check::from_bool(s.contains('@'))),
        "ipv4" => with_str(instance, |s| match ipv4_from_str(s) {
            Ok(_) => Check::Pass,
            Err(e) => Check::Raised(e),
        }),
        "ipv6" => with_str(instance, is_ipv6),
        "date" => with_str(instance, is_date),
        "date-time" => with_str(instance, |s| Check::from_bool(is_datetime(s))),
        "time" => with_str(instance, is_draft3_time),
        "uuid" => with_str(instance, is_uuid),
        _ => return (FormatOutcome::Unknown, None),
    };
    let message = || format!("{} is not a {}", repr(instance), repr_str(format));
    match result {
        Check::Pass => (FormatOutcome::Valid, None),
        Check::Fail => (FormatOutcome::Invalid(message()), None),
        Check::Raised(cause) => (FormatOutcome::Invalid(message()), Some(cause)),
        Check::Crash(err) => (FormatOutcome::Crash(err), None),
    }
}

/// Result of one Python check function.
enum Check {
    /// returned truthy
    Pass,
    /// returned falsy
    Fail,
    /// raised an exception listed in `raises` (becomes the FormatError cause): `str(exc)`
    Raised(String),
    /// raised any other exception: `str(exc)`
    Crash(String),
}

impl Check {
    fn from_bool(ok: bool) -> Self {
        if ok { Check::Pass } else { Check::Fail }
    }
}

/// Every string-based check starts with `if not isinstance(instance, str): return True`.
fn with_str(instance: &Value, check: impl FnOnce(&str) -> Check) -> Check {
    match instance {
        Value::String(s) => check(s),
        _ => Check::Pass,
    }
}

// --- int32 / int64 ---------------------------------------------------------------------------

/// `is_int32` / `is_int64`: bools and non-ints pass; ints must satisfy `~(1 << bits) < x < 1 << bits`.
fn is_int(instance: &Value, bits: u32) -> bool {
    let Value::Number(n) = instance else {
        return true;
    };
    match py_number(n) {
        PyNumber::Int(v) => -(1i128 << bits) <= v && v < (1i128 << bits),
        PyNumber::BigInt(_) => false,
        PyNumber::Float(_) => true,
    }
}

// --- byte ------------------------------------------------------------------------------------

/// `is_byte`: `instance.encode("ascii")` (UnicodeEncodeError is not in `raises` → crash), then
/// `b64decode(..., validate=True)` with binascii errors mapped to `False`.
fn is_byte(s: &str) -> Check {
    if let Some(err) = ascii_encode_error(s) {
        return Check::Crash(err);
    }
    Check::from_bool(a2b_base64_strict(s.as_bytes()))
}

/// `str(UnicodeEncodeError)` for `s.encode("ascii")`: the first run of non-ASCII chars.
fn ascii_encode_error(s: &str) -> Option<String> {
    let start = s.chars().position(|c| !c.is_ascii())?;
    let run = s.chars().skip(start).take_while(|c| !c.is_ascii()).count();
    let reason = "ordinal not in range(128)";
    Some(if run == 1 {
        let bad = s.chars().nth(start).map_or(0, u32::from);
        let escaped = if bad <= 0xff {
            format!("\\x{bad:02x}")
        } else if bad <= 0xffff {
            format!("\\u{bad:04x}")
        } else {
            format!("\\U{bad:08x}")
        };
        format!("'ascii' codec can't encode character '{escaped}' in position {start}: {reason}")
    } else {
        let last = start + run - 1;
        format!("'ascii' codec can't encode characters in position {start}-{last}: {reason}")
    })
}

/// `binascii.a2b_base64(data, strict_mode=True)` succeeds (CPython 3.13 `Modules/binascii.c`).
fn a2b_base64_strict(data: &[u8]) -> bool {
    let mut quad_pos = 0;
    let mut pads = 0;
    for &ch in data {
        if ch == b'=' {
            pads += 1;
            if quad_pos >= 2 && quad_pos + pads <= 4 {
                continue;
            }
            // Leading / excess padding, or a pad after a single data char ("Incorrect" below).
            if quad_pos == 1 {
                break;
            }
            return false;
        }
        if !(ch.is_ascii_alphanumeric() || ch == b'+' || ch == b'/') {
            return false; // "Only base64 data is allowed"
        }
        if pads > 0 {
            return false; // data after padding
        }
        quad_pos = (quad_pos + 1) % 4;
    }
    !(quad_pos == 1 || (quad_pos != 0 && quad_pos + pads < 4))
}

// --- date / date-time ------------------------------------------------------------------------

/// `is_date`: `_RE_DATE.fullmatch(s)` (ASCII `\d{4}-\d{2}-\d{2}`) and `date.fromisoformat(s)`,
/// whose ValueError (in `raises`) becomes the cause.
fn is_date(s: &str) -> Check {
    let b = s.as_bytes();
    let shape = b.len() == 10
        && b.iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        });
    if !shape {
        return Check::Fail;
    }
    let num = |r: std::ops::Range<usize>| s[r].parse::<u32>().unwrap_or(0);
    match check_date_args(num(0..4), num(5..7), num(8..10)) {
        Ok(()) => Check::Pass,
        Err(e) => Check::Raised(e),
    }
}

/// CPython `_datetimemodule.c` `check_date_args`.
fn check_date_args(year: u32, month: u32, day: u32) -> Result<(), String> {
    if !(1..=9999).contains(&year) {
        return Err(format!("year {year} is out of range"));
    }
    if !(1..=12).contains(&month) {
        return Err("month must be in 1..12".to_owned());
    }
    if day < 1 || day > days_in_month(year, month) {
        return Err("day is out of range for month".to_owned());
    }
    Ok(())
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// `is_datetime`: `rfc3339_validator.validate_rfc3339(s.upper())`.
///
/// No non-ASCII char upper-cases into the (ASCII) RFC 3339 alphabet, so any non-ASCII input
/// fails; ASCII `upper()` only maps a-z.
fn is_datetime(s: &str) -> bool {
    if !s.is_ascii() {
        return false;
    }
    let upper = s.to_ascii_uppercase();
    // `$` (no MULTILINE) also matches just before one trailing newline.
    let b = upper.strip_suffix('\n').unwrap_or(&upper).as_bytes();
    let digits = |r: std::ops::Range<usize>| {
        b.get(r.clone())
            .is_some_and(|d| d.iter().all(u8::is_ascii_digit))
            .then(|| {
                b[r].iter()
                    .fold(0u32, |acc, d| acc * 10 + u32::from(d - b'0'))
            })
    };
    let at = |i: usize, c: u8| b.get(i) == Some(&c);
    let (Some(year), Some(month), Some(day)) = (digits(0..4), digits(5..7), digits(8..10)) else {
        return false;
    };
    let (Some(hour), Some(minute), Some(second)) = (digits(11..13), digits(14..16), digits(17..19))
    else {
        return false;
    };
    if !(at(4, b'-') && at(7, b'-') && at(10, b'T') && at(13, b':') && at(16, b':')) {
        return false;
    }
    if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 {
        return false;
    }
    let mut i = 19;
    if at(i, b'.') {
        let frac = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
        if frac == 0 {
            return false;
        }
        i += 1 + frac;
    }
    let zone_ok = match b.get(i) {
        Some(b'Z') => i + 1 == b.len(),
        Some(b'+' | b'-') => {
            let (oh, om) = (digits(i + 1..i + 3), digits(i + 4..i + 6));
            at(i + 3, b':')
                && i + 6 == b.len()
                && oh.is_some_and(|h| h <= 23)
                && om.is_some_and(|m| m <= 59)
        }
        _ => false,
    };
    zone_ok && year != 0 && (1..=days_in_month(year, month)).contains(&day)
}

// --- time (draft 3) ----------------------------------------------------------------------------

/// `is_draft3_time`: `datetime.strptime(s, "%H:%M:%S")`; its ValueError (in `raises`) is the cause.
///
/// The format compiles to `(?P<H>2[0-3]|[0-1]\d|\d| \d):(?P<M>[0-5]\d|\d):(?P<S>6[0-1]|[0-5]\d|\d)`
/// (Unicode `\d`, IGNORECASE, `re.match` without an end anchor).
fn is_draft3_time(s: &str) -> Check {
    let chars: Vec<char> = s.chars().collect();
    let Some((end, second)) = match_hms(&chars) else {
        return Check::Raised(format!(
            "time data {} does not match format '%H:%M:%S'",
            repr_str(s)
        ));
    };
    if end != chars.len() {
        let rest: String = chars[end..].iter().collect();
        return Check::Raised(format!("unconverted data remains: {rest}"));
    }
    if second > 59 {
        return Check::Raised("second must be in 0..59".to_owned());
    }
    Check::Pass
}

/// Backtracking match of the `%H:%M:%S` regex at the start of `chars`; returns the match end
/// and the parsed seconds. Hours and minutes can never be out of range.
fn match_hms(chars: &[char]) -> Option<(usize, u32)> {
    let ch = |i: usize| chars.get(i).copied();
    let dig = |i: usize| ch(i).and_then(decimal_value);
    let in_ascii = |i: usize, lo: char, hi: char| ch(i).is_some_and(|c| (lo..=hi).contains(&c));
    // Each alternative yields (end, value) for a group starting at `p`, in regex order.
    let hours = |p: usize| -> Vec<usize> {
        let mut alts = Vec::new();
        if ch(p) == Some('2') && in_ascii(p + 1, '0', '3') {
            alts.push(p + 2);
        }
        if in_ascii(p, '0', '1') && dig(p + 1).is_some() {
            alts.push(p + 2);
        }
        if dig(p).is_some() {
            alts.push(p + 1);
        }
        if ch(p) == Some(' ') && dig(p + 1).is_some() {
            alts.push(p + 2);
        }
        alts
    };
    let minutes = |p: usize| -> Vec<usize> {
        let mut alts = Vec::new();
        if in_ascii(p, '0', '5') && dig(p + 1).is_some() {
            alts.push(p + 2);
        }
        if dig(p).is_some() {
            alts.push(p + 1);
        }
        alts
    };
    let seconds = |p: usize| -> Vec<usize> {
        let mut alts = Vec::new();
        if ch(p) == Some('6') && in_ascii(p + 1, '0', '1') {
            alts.push(p + 2);
        }
        if in_ascii(p, '0', '5') && dig(p + 1).is_some() {
            alts.push(p + 2);
        }
        if dig(p).is_some() {
            alts.push(p + 1);
        }
        alts
    };
    for h_end in hours(0) {
        if ch(h_end) != Some(':') {
            continue;
        }
        for m_end in minutes(h_end + 1) {
            if ch(m_end) != Some(':') {
                continue;
            }
            if let Some(s_end) = seconds(m_end + 1).into_iter().next() {
                // int() of the group: every char is a decimal digit.
                let second = (m_end + 1..s_end).fold(0, |acc, i| acc * 10 + dig(i).unwrap_or(0));
                return Some((s_end, second));
            }
        }
    }
    None
}

// --- ipv4 / ipv6 -----------------------------------------------------------------------------

/// `ipaddress.IPv4Address(s)`; `Err` is the AddressValueError message.
fn ipv4_from_str(s: &str) -> Result<u32, String> {
    if s.contains('/') {
        return Err(format!("Unexpected '/' in {}", repr_str(s)));
    }
    ipv4_int_from_string(s)
}

/// `_BaseV4._ip_int_from_string`.
fn ipv4_int_from_string(s: &str) -> Result<u32, String> {
    if s.is_empty() {
        return Err("Address cannot be empty".to_owned());
    }
    let octets: Vec<&str> = s.split('.').collect();
    if octets.len() != 4 {
        return Err(format!("Expected 4 octets in {}", repr_str(s)));
    }
    let mut ip = 0u32;
    for octet in octets {
        let value = parse_octet(octet).map_err(|e| format!("{e} in {}", repr_str(s)))?;
        ip = (ip << 8) | value;
    }
    Ok(ip)
}

/// `_BaseV4._parse_octet`.
fn parse_octet(octet: &str) -> Result<u32, String> {
    if octet.is_empty() {
        return Err("Empty octet not permitted".to_owned());
    }
    if !octet.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "Only decimal digits permitted in {}",
            repr_str(octet)
        ));
    }
    if octet.len() > 3 {
        return Err(format!(
            "At most 3 characters permitted in {}",
            repr_str(octet)
        ));
    }
    if octet != "0" && octet.starts_with('0') {
        return Err(format!(
            "Leading zeros are not permitted in {}",
            repr_str(octet)
        ));
    }
    let value: u32 = octet.parse().unwrap_or(u32::MAX);
    if value > 255 {
        return Err(format!("Octet {value} (> 255) not permitted"));
    }
    Ok(value)
}

/// `is_ipv6`: `IPv6Address(s)` (AddressValueError is the cause), then `not address.scope_id`.
fn is_ipv6(s: &str) -> Check {
    if s.contains('/') {
        return Check::Raised(format!("Unexpected '/' in {}", repr_str(s)));
    }
    // `_split_scope_id`
    let (addr, scope_id) = match s.split_once('%') {
        None => (s, None),
        Some((_, scope)) if scope.is_empty() || scope.contains('%') => {
            return Check::Raised(format!("Invalid IPv6 address: \"{}\"", repr_str(s)));
        }
        Some((addr, scope)) => (addr, Some(scope)),
    };
    if let Err(e) = ipv6_int_from_string(addr) {
        return Check::Raised(e);
    }
    Check::from_bool(scope_id.is_none())
}

/// `_BaseV6._ip_int_from_string` (validation only).
fn ipv6_int_from_string(ip: &str) -> Result<(), String> {
    const HEXTETS: usize = 8;
    if ip.is_empty() {
        return Err("Address cannot be empty".to_owned());
    }
    let len = ip.chars().count();
    if len > 45 {
        let shorten = if len > 100 {
            let head: String = ip.chars().take(45).collect();
            let tail: String = ip.chars().skip(len - 45).collect();
            format!("{head}({} chars elided){tail}", len - 90)
        } else {
            ip.to_owned()
        };
        return Err(format!(
            "At most 45 characters expected in {}",
            repr_str(&shorten)
        ));
    }
    // `ip.split(':', maxsplit=9)`
    let mut parts: Vec<String> = ip.splitn(HEXTETS + 2, ':').map(str::to_owned).collect();
    if parts.len() < 3 {
        return Err(format!("At least 3 parts expected in {}", repr_str(ip)));
    }
    if parts.last().is_some_and(|p| p.contains('.')) {
        let tail = parts.pop().unwrap_or_default();
        let v4 = ipv4_from_str(&tail).map_err(|e| format!("{e} in {}", repr_str(ip)))?;
        parts.push(format!("{:x}", v4 >> 16));
        parts.push(format!("{:x}", v4 & 0xffff));
    }
    if parts.len() > HEXTETS + 1 {
        return Err(format!(
            "At most {HEXTETS} colons permitted in {}",
            repr_str(ip)
        ));
    }
    let mut skip_index = None;
    for (i, part) in parts.iter().enumerate().take(parts.len() - 1).skip(1) {
        if part.is_empty() {
            if skip_index.is_some() {
                return Err(format!("At most one '::' permitted in {}", repr_str(ip)));
            }
            skip_index = Some(i);
        }
    }
    let first_empty = parts[0].is_empty();
    let last_empty = parts[parts.len() - 1].is_empty();
    let (parts_hi, parts_lo) = if let Some(skip) = skip_index {
        let mut hi = skip;
        let mut lo = parts.len() - skip - 1;
        if first_empty {
            hi -= 1;
            if hi > 0 {
                return Err(format!(
                    "Leading ':' only permitted as part of '::' in {}",
                    repr_str(ip)
                ));
            }
        }
        if last_empty {
            lo -= 1;
            if lo > 0 {
                return Err(format!(
                    "Trailing ':' only permitted as part of '::' in {}",
                    repr_str(ip)
                ));
            }
        }
        if hi + lo >= HEXTETS {
            return Err(format!(
                "Expected at most {} other parts with '::' in {}",
                HEXTETS - 1,
                repr_str(ip)
            ));
        }
        (hi, lo)
    } else {
        if parts.len() != HEXTETS {
            return Err(format!(
                "Exactly {HEXTETS} parts expected without '::' in {}",
                repr_str(ip)
            ));
        }
        if first_empty {
            return Err(format!(
                "Leading ':' only permitted as part of '::' in {}",
                repr_str(ip)
            ));
        }
        if last_empty {
            return Err(format!(
                "Trailing ':' only permitted as part of '::' in {}",
                repr_str(ip)
            ));
        }
        (parts.len(), 0)
    };
    let n = parts.len();
    for part in parts[..parts_hi].iter().chain(&parts[n - parts_lo..]) {
        parse_hextet(part).map_err(|e| format!("{e} in {}", repr_str(ip)))?;
    }
    Ok(())
}

/// `_BaseV6._parse_hextet`.
fn parse_hextet(hextet: &str) -> Result<(), String> {
    if !hextet.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("Only hex digits permitted in {}", repr_str(hextet)));
    }
    if hextet.len() > 4 {
        return Err(format!(
            "At most 4 characters permitted in {}",
            repr_str(hextet)
        ));
    }
    if hextet.is_empty() {
        return Err("invalid literal for int() with base 16: ''".to_owned());
    }
    Ok(())
}

// --- uuid ------------------------------------------------------------------------------------

/// `is_uuid`: `UUID(s)` (its ValueError is the cause), then hyphens at positions 8/13/18/23.
fn is_uuid(s: &str) -> Check {
    let replaced = s.replace("urn:", "").replace("uuid:", "");
    let hex = replaced.trim_matches(['{', '}']).replace('-', "");
    if hex.chars().count() != 32 {
        return Check::Raised("badly formed hexadecimal UUID string".to_owned());
    }
    match py_int_base16(&hex) {
        None => {
            let rep: String = repr_str(&hex).chars().take(200).collect(); // "%.200R"
            return Check::Raised(format!("invalid literal for int() with base 16: {rep}"));
        }
        Some(true) => {
            return Check::Raised("int is out of range (need a 128-bit value)".to_owned());
        }
        Some(false) => {}
    }
    // len(s) >= 32 here, so the indexing can't raise IndexError.
    let chars: Vec<char> = s.chars().collect();
    Check::from_bool([8, 13, 18, 23].iter().all(|&i| chars.get(i) == Some(&'-')))
}

/// `int(s, 16)` (CPython `PyLong_FromUnicodeObject`): `None` on ValueError, otherwise whether
/// the result is negative (and non-zero).
fn py_int_base16(s: &str) -> Option<bool> {
    // `_PyUnicode_TransformDecimalAndSpaceToASCII`: non-ASCII whitespace -> ' ', decimal
    // digits -> ASCII; anything else non-ASCII is invalid. An embedded NUL ends the C string
    // early, which then fails the "consumed everything" check.
    let mut ascii = Vec::with_capacity(s.len());
    for c in s.chars() {
        let b = if (c as u32) < 127 {
            c as u8
        } else if py_isspace(c) {
            b' '
        } else {
            b'0' + u8::try_from(decimal_value(c)?).ok()?
        };
        if b == 0 {
            return None;
        }
        ascii.push(b);
    }
    // `PyLong_FromString`
    let is_space = |b: u8| matches!(b, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r');
    let mut p = ascii.iter().take_while(|b| is_space(**b)).count();
    let at = |i: usize| ascii.get(i).copied().unwrap_or(0);
    let negative = match at(p) {
        b'+' => {
            p += 1;
            false
        }
        b'-' => {
            p += 1;
            true
        }
        _ => false,
    };
    if at(p) == b'0' && matches!(at(p + 1), b'x' | b'X') {
        p += 2;
        if at(p) == b'_' {
            p += 1; // one underscore allowed after the prefix
        }
    }
    // `long_from_string_base`: digits with single underscores between them.
    if at(p) == b'_' {
        return None;
    }
    let (mut digits, mut nonzero, mut prev) = (0, false, 0u8);
    while at(p).is_ascii_hexdigit() || at(p) == b'_' {
        let b = at(p);
        if b == b'_' {
            if prev == b'_' {
                return None;
            }
        } else {
            digits += 1;
            nonzero |= b != b'0';
        }
        prev = b;
        p += 1;
    }
    if prev == b'_' || digits == 0 {
        return None;
    }
    while is_space(at(p)) {
        p += 1;
    }
    if p != ascii.len() {
        return None;
    }
    Some(negative && nonzero)
}

/// Value of a Unicode decimal digit (`unicodedata.decimal`), Unicode 15.1.
fn decimal_value(c: char) -> Option<u32> {
    let cp = c as u32;
    let i = DECIMAL_ZEROS.partition_point(|&z| z <= cp);
    let zero = DECIMAL_ZEROS[..i].last()?;
    (cp - zero < 10).then_some(cp - zero)
}

// Generated by gen/gen_tables.py from CPython 3.13.14.
/// Code points of every Unicode decimal-digit ZERO (Nd, Unicode 15.1.0); each starts a run of ten digits 0..=9.
static DECIMAL_ZEROS: &[u32] = &[
    0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
    0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
    0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0, 0xff10,
    0x104a0, 0x10d30, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0, 0x11650,
    0x116c0, 0x11730, 0x118e0, 0x11950, 0x11c50, 0x11d50, 0x11da0, 0x11f50, 0x16a60, 0x16ac0,
    0x16b50, 0x1d7ce, 0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e950,
    0x1fbf0,
];
