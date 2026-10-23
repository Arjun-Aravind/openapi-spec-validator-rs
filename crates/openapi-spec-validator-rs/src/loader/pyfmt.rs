//! Python text formatting the loader needs to reproduce `str(exc)` and `json.dumps` output.

use std::fmt::Write as _;

/// A libyaml/PyYAML position. Python prints `line + 1` and `column + 1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(super) struct Mark {
    pub line: u64,
    pub column: u64,
}

/// `MarkedYAMLError.__str__` with PyYAML's C-extension `Mark` (which has no snippet).
pub(super) fn marked_error(
    name: &str,
    context: Option<&str>,
    context_mark: Option<Mark>,
    problem: Option<&str>,
    problem_mark: Option<Mark>,
) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(4);
    if let Some(context) = context {
        lines.push(context.to_owned());
    }
    if let Some(cm) = context_mark {
        // The context mark is omitted when it points at the problem position.
        let same = problem.is_some() && problem_mark.is_some_and(|pm| pm == cm);
        if !same {
            lines.push(mark_line(name, cm));
        }
    }
    if let Some(problem) = problem {
        lines.push(problem.to_owned());
    }
    if let Some(pm) = problem_mark {
        lines.push(mark_line(name, pm));
    }
    lines.join("\n")
}

fn mark_line(name: &str, mark: Mark) -> String {
    format!(
        "  in \"{name}\", line {}, column {}",
        mark.line + 1,
        mark.column + 1
    )
}

/// `'%04x' % value` (Python keeps the sign inside the zero-padded width).
pub(super) fn hex04(value: i64) -> String {
    if value < 0 {
        format!("-{:03x}", value.unsigned_abs())
    } else {
        format!("{value:04x}")
    }
}

/// `str.isspace()`.
pub(super) fn is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n'
            | '\u{0b}'
            | '\u{0c}'
            | '\r'
            | '\u{1c}'..='\u{1f}'
            | ' '
            | '\u{85}'
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

/// `str.strip()` without arguments.
pub(super) fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `str.isprintable()` for a single character.
///
/// Exact for ASCII, Latin-1 and the format/separator/private-use/noncharacter ranges; code points
/// that are merely unassigned in Unicode 15.1 are treated as printable.
fn is_printable(c: char) -> bool {
    let u = c as u32;
    if u < 0x20 || u == 0x7f {
        return false;
    }
    if u < 0x7f {
        return true;
    }
    if (u & 0xfffe) == 0xfffe {
        return false;
    }
    !matches!(
        u,
        0x80..=0xa0
            | 0xad
            | 0x600..=0x605
            | 0x61c
            | 0x6dd
            | 0x70f
            | 0x890..=0x891
            | 0x8e2
            | 0x180e
            | 0x1680
            | 0x2000..=0x200f
            | 0x2028..=0x202f
            | 0x205f..=0x206f
            | 0x3000
            | 0xd800..=0xf8ff
            | 0xfdd0..=0xfdef
            | 0xfeff
            | 0xfff9..=0xfffb
            | 0x110bd
            | 0x110cd
            | 0x13430..=0x1343f
            | 0x1bca0..=0x1bca3
            | 0x1d173..=0x1d17a
            | 0xe0001
            | 0xe0020..=0xe007f
            | 0xf0000..
    )
}

/// `repr(s)` for a `str`.
pub(super) fn repr_str(s: &str) -> String {
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
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if is_printable(c) => out.push(c),
            c => push_escaped_char(&mut out, c as u32),
        }
    }
    out.push(quote);
    out
}

/// The `\xNN` / `\uNNNN` / `\UNNNNNNNN` spelling Python uses for unprintable characters.
pub(super) fn push_escaped_char(out: &mut String, u: u32) {
    // Writing to a String cannot fail.
    let _ = if u <= 0xff {
        write!(out, "\\x{u:02x}")
    } else if u <= 0xffff {
        write!(out, "\\u{u:04x}")
    } else {
        write!(out, "\\U{u:08x}")
    };
}

/// `'%.200R' % s`: the repr, truncated to 200 characters (as `int()` errors do).
pub(super) fn repr_str_200(s: &str) -> String {
    let repr = repr_str(s);
    match repr.char_indices().nth(200) {
        Some((cut, _)) => repr[..cut].to_owned(),
        None => repr,
    }
}

/// `float.__repr__` for a finite float.
pub(super) fn float_repr(f: f64) -> String {
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0" } else { "0.0" }.to_owned();
    }
    // Rust's `{:e}` yields the same shortest round-tripping digits as Python's repr.
    let sci = format!("{:e}", f.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let decpt = exp + 1;
    let mut out = String::new();
    if f < 0.0 {
        out.push('-');
    }
    let ndigits = i32::try_from(digits.len()).unwrap_or(i32::MAX);
    if decpt <= -4 || decpt > 16 {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let e = decpt - 1;
        let _ = write!(
            out,
            "e{}{:02}",
            if e < 0 { '-' } else { '+' },
            e.unsigned_abs()
        );
    } else if decpt <= 0 {
        out.push_str("0.");
        for _ in 0..-decpt {
            out.push('0');
        }
        out.push_str(&digits);
    } else if decpt >= ndigits {
        out.push_str(&digits);
        for _ in 0..decpt - ndigits {
            out.push('0');
        }
        out.push_str(".0");
    } else {
        let split = usize::try_from(decpt).unwrap_or(0);
        out.push_str(&digits[..split]);
        out.push('.');
        out.push_str(&digits[split..]);
    }
    out
}

/// How `json.dumps` spells a float (keys included): `repr`, or the JavaScript names for
/// non-finite values.
pub(super) fn json_float(f: f64) -> String {
    if f.is_nan() {
        "NaN".to_owned()
    } else if f.is_infinite() {
        if f > 0.0 { "Infinity" } else { "-Infinity" }.to_owned()
    } else {
        float_repr(f)
    }
}

/// `str(UnicodeEncodeError)` for a run of unencodable characters `[start, end)`.
pub(super) fn unicode_encode_error(
    codec: &str,
    chars: &[char],
    start: usize,
    end: usize,
    reason: &str,
) -> String {
    if end == start + 1 {
        let mut c = String::new();
        push_escaped_char(&mut c, chars[start] as u32);
        format!("'{codec}' codec can't encode character '{c}' in position {start}: {reason}")
    } else {
        format!(
            "'{codec}' codec can't encode characters in position {start}-{}: {reason}",
            end - 1
        )
    }
}

/// `str(UnicodeDecodeError)` for bytes `[start, end)` of `input`.
pub(super) fn unicode_decode_error(input: &[u8], start: usize, end: usize, reason: &str) -> String {
    if end == start + 1 {
        format!(
            "'utf-8' codec can't decode byte 0x{:02x} in position {start}: {reason}",
            input[start]
        )
    } else {
        format!(
            "'utf-8' codec can't decode bytes in position {start}-{}: {reason}",
            end - 1
        )
    }
}

/// `str.lower()` (full Unicode lowercase mapping, like Python).
pub(super) fn lower(s: &str) -> String {
    s.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_repr_matches_python() {
        let cases = [
            (0.1, "0.1"),
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (1e-5, "1e-05"),
            (1e-4, "0.0001"),
            (5e-324, "5e-324"),
            (1.797_693_134_862_315_7e308, "1.7976931348623157e+308"),
            (123_456_789.123, "123456789.123"),
            (2.5, "2.5"),
            (100.0, "100.0"),
            (1e22, "1e+22"),
            (9_007_199_254_740_992.0, "9007199254740992.0"),
            (0.300_000_000_000_000_04, "0.30000000000000004"),
            (-1.5e-7, "-1.5e-07"),
            (-0.0, "-0.0"),
            (6.02e23, "6.02e+23"),
        ];
        for (f, want) in cases {
            assert_eq!(float_repr(f), want, "{f:e}");
        }
    }

    #[test]
    fn repr_quotes_like_python() {
        assert_eq!(repr_str("it's"), "\"it's\"");
        assert_eq!(repr_str("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(repr_str("é\u{85}\t"), "'é\\x85\\t'");
        assert_eq!(hex04(-1), "-001");
        assert_eq!(hex04(0xff), "00ff");
    }
}
