//! The few `str` methods and `unicodedata` services `_parser.py` relies on.
//!
//! Non-ASCII character properties come from the Unicode tables bundled with
//! `regex-syntax` (via fancy-regex), restricted with `\p{Age=15.1}` to the
//! characters CPython 3.13's Unicode 15.1.0 database knows.

use std::sync::OnceLock;

use fancy_regex::Regex;

/// Lazily compiled property matchers for non-ASCII input.
struct Props {
    identifier: Option<Regex>,
    alpha: Option<Regex>,
    unprintable: Option<Regex>,
}

fn props() -> &'static Props {
    static PROPS: OnceLock<Props> = OnceLock::new();
    PROPS.get_or_init(|| Props {
        identifier: Regex::new(
            r"\A[[\p{XID_Start}_]&&\p{Age=15.1}][\p{XID_Continue}&&\p{Age=15.1}]*\z",
        )
        .ok(),
        alpha: Regex::new(r"\A[\p{L}&&\p{Age=15.1}]\z").ok(),
        // Cn as of Unicode 15.1 includes everything assigned later.
        unprintable: Regex::new(r"\A[\p{Cc}\p{Cf}\p{Co}\p{Cn}\p{Zl}\p{Zp}\p{Zs}[^\p{Age=15.1}]]\z")
            .ok(),
    })
}

fn char_matches(re: Option<&Regex>, c: char) -> Option<bool> {
    let mut buf = [0u8; 4];
    re?.is_match(c.encode_utf8(&mut buf)).ok()
}

/// `str.isidentifier()`
pub(crate) fn is_identifier(s: &str) -> bool {
    if s.is_ascii() {
        let mut bytes = s.bytes();
        return bytes
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_');
    }
    props()
        .identifier
        .as_ref()
        .and_then(|re| re.is_match(s).ok())
        .unwrap_or(false)
}

/// `str.isalpha()` for a single character.
pub(crate) fn py_isalpha(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphabetic();
    }
    char_matches(props().alpha.as_ref(), c).unwrap_or_else(|| c.is_alphabetic())
}

/// `str.isprintable()` for a single non-ASCII character.
fn is_printable(c: char) -> bool {
    !char_matches(props().unprintable.as_ref(), c).unwrap_or(false)
}

/// `repr(s)` for a Python `str`.
pub(crate) fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            _ if c == quote || c == '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ if c < ' ' || c == '\x7f' => out.push_str(&format!("\\x{:02x}", c as u32)),
            _ if c.is_ascii() || is_printable(c) => out.push(c),
            _ if (c as u32) <= 0xff => out.push_str(&format!("\\x{:02x}", c as u32)),
            _ if (c as u32) <= 0xffff => out.push_str(&format!("\\u{:04x}", c as u32)),
            _ => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(quote);
    out
}

/// Marker code point for a `\N{...}` name that looks valid but cannot be
/// resolved without the Unicode name database (outside the Unicode range).
pub(crate) const UNRESOLVED_NAME: u32 = 0x11_0000;

/// `unicodedata.lookup(name)` as used by `\N{...}`.
///
/// The full name table (~33k explicit names, ~0.9 MB) is deliberately not
/// embedded. Names that cannot possibly be valid (characters outside the
/// Unicode name alphabet, not starting with a letter, or longer than CPython's
/// `NAME_MAXLEN`) are rejected like CPython; any other name is assumed to exist and yields
/// [`UNRESOLVED_NAME`], which makes the pattern unmatchable by
/// [`super::PyRegex::search`].
pub(crate) fn lookup_name(name: &str) -> Option<u32> {
    const NAME_MAXLEN: usize = 256;
    let plausible = name.len() <= NAME_MAXLEN
        && name.starts_with(|c: char| c.is_ascii_alphabetic())
        && !name.ends_with(' ')
        && !name.contains("  ")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b' ' || b == b'-');
    plausible.then_some(UNRESOLVED_NAME)
}
