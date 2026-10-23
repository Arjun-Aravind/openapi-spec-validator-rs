//! Fast path: valid JSON that PyYAML would load to the same value is parsed by serde_json.
//!
//! JSON is (almost) a YAML flow document, but libyaml and PyYAML's resolvers disagree with
//! JSON in a few places. `scan` walks the already-validated JSON and declines (sending the
//! input down the faithful YAML path) when it sees any of them:
//!
//! * characters libyaml's reader rejects (DEL, C1 controls, U+FFFE/U+FFFF) or treats as line
//!   breaks inside quoted scalars (NEL, U+2028, U+2029);
//! * `\uD800`-`\uDFFF` escapes: libyaml rejects surrogate escapes, even as valid pairs;
//! * a tab outside any `{}`/`[]` (libyaml only skips tabs in flow context or after a token);
//! * an object key that is not a valid simple key: its closing quote and the `:` must be on
//!   one line, and the key plus following whitespace must span at most 1024 characters;
//!
//! and it records the numbers where serde_json's value differs from Python's: `-0` (Python's
//! int 0) and floats whose serde_json (non-`float_roundtrip`) parse may be off by an ulp.

use serde_json::{Number, Value};

/// libyaml drops a simple-key candidate once the scanner is more than 1024 characters past
/// its start; with the opening quote at offset 0, the `:` must be at offset <= 1024.
const MAX_SIMPLE_KEY_SPAN: usize = 1022;

/// A correction for one number token that serde_json parsed as `f64`.
enum Fixup {
    Keep,
    Replace(Value),
}

struct Scan {
    keys: usize,
    /// One entry per number that serde_json stores as `f64`, in document order.
    fixups: Vec<Fixup>,
}

pub(super) fn try_load(bytes: &[u8]) -> Option<Value> {
    let mut value: Value = serde_json::from_slice(bytes).ok()?;
    let scan = scan(bytes)?;
    if scan.fixups.iter().any(|f| matches!(f, Fixup::Replace(_))) {
        // Fixups are matched to values by document order, which only holds when no duplicate
        // key dropped a value.
        if count_entries(&value) != scan.keys {
            return None;
        }
        let mut fixups = scan.fixups.into_iter();
        apply(&mut value, &mut fixups);
    }
    Some(value)
}

fn scan(b: &[u8]) -> Option<Scan> {
    let mut out = Scan {
        keys: 0,
        fixups: Vec::new(),
    };
    // true = object
    let mut stack: Vec<bool> = Vec::new();
    let mut expect_key = false;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b' ' | b'\n' | b'\r' => i += 1,
            b'\t' => {
                if stack.is_empty() {
                    return None;
                }
                i += 1;
            }
            b'{' => {
                stack.push(true);
                expect_key = true;
                i += 1;
            }
            b'[' => {
                stack.push(false);
                expect_key = false;
                i += 1;
            }
            b'}' | b']' => {
                stack.pop();
                expect_key = false;
                i += 1;
            }
            b',' => {
                expect_key = stack.last() == Some(&true);
                i += 1;
            }
            b':' => {
                expect_key = false;
                i += 1;
            }
            b'"' => {
                let (end, chars) = scan_string(b, i + 1)?;
                i = end;
                if expect_key {
                    let mut ws = 0;
                    loop {
                        match b.get(i) {
                            Some(b' ' | b'\t') => {
                                ws += 1;
                                i += 1;
                            }
                            Some(b':') => break,
                            _ => return None,
                        }
                    }
                    if chars + ws > MAX_SIMPLE_KEY_SPAN {
                        return None;
                    }
                    out.keys += 1;
                    expect_key = false;
                }
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                while i < b.len() && matches!(b[i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
                {
                    i += 1;
                }
                if let Some(fixup) = classify_number(&b[start..i]) {
                    out.fixups.push(fixup);
                }
            }
            b't' | b'n' => i += 4,
            b'f' => i += 5,
            _ => return None,
        }
    }
    Some(out)
}

/// Scans a string body starting after the opening quote. Returns the index after the closing
/// quote and the number of source characters between the quotes.
fn scan_string(b: &[u8], mut i: usize) -> Option<(usize, usize)> {
    let mut chars = 0;
    loop {
        let c = *b.get(i)?;
        match c {
            b'"' => return Some((i + 1, chars)),
            b'\\' => {
                if b.get(i + 1) == Some(&b'u') {
                    let hex = b.get(i + 2..i + 6)?;
                    let first = char::from(hex[0]).to_ascii_lowercase();
                    let second = char::from(hex[1]).to_ascii_lowercase();
                    if first == 'd' && matches!(second, '8'..='9' | 'a'..='f') {
                        return None;
                    }
                    chars += 6;
                    i += 6;
                } else {
                    chars += 2;
                    i += 2;
                }
            }
            0x7f => return None,
            0xc2 if matches!(b.get(i + 1), Some(0x80..=0x9f)) => return None,
            0xe2 if b.get(i + 1) == Some(&0x80) && matches!(b.get(i + 2), Some(0xa8 | 0xa9)) => {
                return None;
            }
            0xef if b.get(i + 1) == Some(&0xbf) && matches!(b.get(i + 2), Some(0xbe | 0xbf)) => {
                return None;
            }
            _ => {
                if c & 0xc0 != 0x80 {
                    chars += 1;
                }
                i += 1;
            }
        }
    }
}

/// `None` when serde_json does not produce an `f64` for this token.
fn classify_number(tok: &[u8]) -> Option<Fixup> {
    let negative = tok.first() == Some(&b'-');
    let body = if negative { &tok[1..] } else { tok };
    let is_float = body.iter().any(|&c| matches!(c, b'.' | b'e' | b'E'));
    if !is_float {
        if body == b"0" {
            // serde_json reads `-0` as -0.0; YAML resolves it to the int 0.
            return negative.then(|| Fixup::Replace(Value::Number(0u64.into())));
        }
        let fits = std::str::from_utf8(body)
            .ok()
            .and_then(|d| d.parse::<u64>().ok())
            .is_some_and(|m| !negative || m <= 1 << 63);
        // Larger integers become serde_json's own f64 on both paths.
        return (!fits).then_some(Fixup::Keep);
    }
    if exact_in_serde(body) {
        return Some(Fixup::Keep);
    }
    let text = std::str::from_utf8(tok).ok()?;
    let f: f64 = text.parse().ok()?;
    Some(Fixup::Replace(
        Number::from_f64(f).map_or(Value::Null, Value::Number),
    ))
}

/// serde_json's default float parser computes `significand * 10^exp` (or `/`) in one rounding
/// step, which is exact when the significand fits 53 bits and `10^|exp|` is a double.
fn exact_in_serde(body: &[u8]) -> bool {
    let (mantissa, exponent) = match body.iter().position(|&c| c == b'e' || c == b'E') {
        Some(p) => (&body[..p], &body[p + 1..]),
        None => (body, &b""[..]),
    };
    let explicit: i64 = match std::str::from_utf8(exponent) {
        Ok("") => 0,
        Ok(e) => match e.parse() {
            Ok(v) => v,
            Err(_) => return false,
        },
        Err(_) => return false,
    };
    let frac_digits = mantissa
        .iter()
        .position(|&c| c == b'.')
        .map_or(0, |p| mantissa.len() - p - 1);
    let mut significand: u64 = 0;
    for &c in mantissa.iter().filter(|c| c.is_ascii_digit()) {
        significand = match significand
            .checked_mul(10)
            .and_then(|s| s.checked_add(u64::from(c - b'0')))
        {
            Some(s) => s,
            None => return false,
        };
    }
    if significand == 0 {
        return true;
    }
    let exp = explicit - i64::try_from(frac_digits).unwrap_or(i64::MAX);
    significand <= 1 << 53 && (-22..=22).contains(&exp)
}

fn count_entries(value: &Value) -> usize {
    let mut stack = vec![value];
    let mut n = 0;
    while let Some(v) = stack.pop() {
        match v {
            Value::Object(map) => {
                n += map.len();
                stack.extend(map.values());
            }
            Value::Array(items) => stack.extend(items),
            _ => {}
        }
    }
    n
}

/// Applies fixups to the `f64` numbers in document (pre-)order.
fn apply(value: &mut Value, fixups: &mut impl Iterator<Item = Fixup>) {
    let mut stack = vec![value];
    while let Some(v) = stack.pop() {
        if matches!(v, Value::Number(n) if n.is_f64()) {
            if let Some(Fixup::Replace(new)) = fixups.next() {
                *v = new;
            }
            continue;
        }
        match v {
            // Pushed in reverse so that children pop in document order.
            Value::Object(map) => stack.extend(map.values_mut().rev()),
            Value::Array(items) => stack.extend(items.iter_mut().rev()),
            _ => {}
        }
    }
}
