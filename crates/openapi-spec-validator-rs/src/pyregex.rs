//! CPython 3.13 `re` compatibility for `str` patterns.
//!
//! [`PyRegex::compile`] accepts exactly the patterns `re.compile(pattern)`
//! accepts, reporting the same error message and position (it is a port of
//! `re/_parser.py` plus the look-behind checks of `re/_compiler.py`).
//! [`PyRegex::search`] answers `re.search(pattern, text) is not None` by
//! translating the parsed pattern to an equivalent fancy-regex.
//!
//! Unicode properties (`\w`, `\d`, identifiers in group names, printable
//! characters in messages) are pinned to CPython 3.13's Unicode 15.1.
//!
//! Known gaps:
//! - `\N{NAME}`: no Unicode name table is embedded. Impossible names are
//!   rejected exactly; plausible ones are accepted, and `search` returns `Err`.
//! - `RecursionError` depends on the caller's Python stack depth; the limit is
//!   calibrated on openapi-spec-validator 0.9.0 (485 nested groups compile,
//!   486 raise).
//! - `search` returns `Err` for patterns nested deeper than 64 levels, and for
//!   a conditional `(?(g)...)` placed inside group `g` itself.
//! - Case-insensitive backreferences compare by simple case folding, where
//!   `_sre` compares `str.lower()` (differs for e.g. `İ`/`i`, `ſ`/`s`, `µ`/`μ`).

mod ast;
mod parser;
mod translate;
mod unicode;

use std::fmt;
use std::sync::OnceLock;

/// The Python exception class `re.compile` raises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// `re.error` (`re.PatternError`): the only kind jsonschema's `regex`
    /// format checker catches; all others propagate as crashes upstream.
    Re,
    /// `OverflowError`, e.g. "the repetition number is too large".
    Overflow,
    /// `ValueError`, e.g. "ASCII and UNICODE flags are incompatible" for
    /// `(?a)(?u)`, or the int/str conversion digit limit.
    Value,
    /// `RecursionError` for deeply nested groups.
    Recursion,
    /// `TypeError` (only reachable with more than `MAXGROUPS` unnamed groups).
    Type,
}

impl ErrorKind {
    /// The qualified Python exception name.
    pub fn python_name(self) -> &'static str {
        match self {
            ErrorKind::Re => "re.PatternError",
            ErrorKind::Overflow => "OverflowError",
            ErrorKind::Value => "ValueError",
            ErrorKind::Recursion => "RecursionError",
            ErrorKind::Type => "TypeError",
        }
    }
}

/// `re.error` details, matching CPython's message (e.g. "unterminated character
/// set at position 0"). `pos` is a character (code point) index, as in Python.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyRegexError {
    /// Which exception CPython raises; anything but [`ErrorKind::Re`] is not a `re.error`.
    pub kind: ErrorKind,
    /// The unformatted message (`err.msg`).
    pub msg: String,
    /// `err.pos`; `None` for errors raised without a position (look-behind
    /// width errors and the non-`re.error` kinds).
    pub pos: Option<usize>,
}

impl PyRegexError {
    /// `str(err)` as CPython formats it for `pattern`, including the
    /// `(line L, column C)` suffix for multi-line patterns.
    pub fn python_str(&self, pattern: &str) -> String {
        let Some(pos) = self.pos.filter(|_| self.kind == ErrorKind::Re) else {
            return self.msg.clone();
        };
        let mut s = format!("{} at position {pos}", self.msg);
        if pattern.contains('\n') {
            let before: Vec<char> = pattern.chars().take(pos).collect();
            let lineno = before.iter().filter(|&&c| c == '\n').count() + 1;
            let colno = match before.iter().rposition(|&c| c == '\n') {
                Some(nl) => pos - nl,
                None => pos + 1,
            };
            s.push_str(&format!(" (line {lineno}, column {colno})"));
        }
        s
    }
}

/// `str(err)` without the line/column suffix (see [`PyRegexError::python_str`]);
/// exceptions other than `re.error` are prefixed with their class name.
impl fmt::Display for PyRegexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.kind, self.pos) {
            (ErrorKind::Re, Some(pos)) => write!(f, "{} at position {pos}", self.msg),
            (ErrorKind::Re, None) => f.write_str(&self.msg),
            (kind, _) => write!(f, "{}: {}", kind.python_name(), self.msg),
        }
    }
}

impl std::error::Error for PyRegexError {}

/// A pattern CPython's `re` accepts, translated for matching.
pub struct PyRegex {
    pattern: String,
    parsed: parser::Parsed,
    /// Built on first `search`: translating and compiling costs far more than
    /// validating, and most callers only validate.
    matcher: OnceLock<Result<fancy_regex::Regex, String>>,
}

impl fmt::Debug for PyRegex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PyRegex")
            .field("pattern", &self.pattern)
            .finish_non_exhaustive()
    }
}

impl PyRegex {
    /// Equivalent of `re.compile(pattern)` (no flags): `Err` exactly when
    /// CPython 3.13 raises (see [`ErrorKind`] for which exception).
    pub fn compile(pattern: &str) -> Result<PyRegex, PyRegexError> {
        let parsed = parser::compile(pattern)?;
        Ok(PyRegex {
            pattern: pattern.to_owned(),
            parsed,
            matcher: OnceLock::new(),
        })
    }

    /// The fancy-regex pattern used by [`PyRegex::search`], or why there is none.
    #[cfg(test)]
    pub fn translated(&self) -> Result<String, String> {
        match self.matcher() {
            Ok(re) => Ok(re.as_str().to_owned()),
            Err(e) => Err(e.clone()),
        }
    }

    fn matcher(&self) -> &Result<fancy_regex::Regex, String> {
        self.matcher.get_or_init(|| {
            let build = |harden| {
                let translated = translate::translate(&self.parsed, harden)?;
                guard(|| fancy_regex::Regex::new(&translated).map_err(|e| e.to_string()))
            };
            // The hardened form trades the delegate engine's speed for not
            // unrolling large counted repeats; only use it when needed.
            build(false).or_else(|err| build(true).map_err(|_| err))
        })
    }

    /// Equivalent of `re.search(pattern, text) is not None`. `Err` only if the
    /// translated pattern cannot be evaluated (unsupported construct, engine
    /// limits such as the backtrack limit).
    pub fn search(&self, text: &str) -> Result<bool, String> {
        match self.matcher() {
            Ok(re) => guard(|| re.is_match(text).map_err(|e| e.to_string())),
            Err(e) => Err(e.clone()),
        }
    }
}

/// Runs a fancy-regex call, turning an engine panic into an error so that no
/// input can bring the caller down (the translated patterns stay within what
/// the engine supports, so this is defence in depth).
fn guard<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err("internal error in the regex engine".to_owned()))
}

/// Cheap validity check: `re.compile(pattern)` succeeds.
#[cfg(test)]
pub fn is_valid(pattern: &str) -> bool {
    parser::compile(pattern).is_ok()
}

/// openapi-schema-validator's `regex` format check: `Ok(false)` when `re.compile` raises
/// `re.error`, `Err(str(exc))` for the other exceptions it can raise, which the check does
/// not catch and which therefore abort validation upstream.
pub fn check_regex_format(pattern: &str) -> Result<bool, String> {
    match parser::compile(pattern) {
        Ok(_) => Ok(true),
        Err(error) if error.kind == ErrorKind::Re => Ok(false),
        Err(error) => Err(error.python_str(pattern)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(pattern: &str) -> PyRegexError {
        PyRegex::compile(pattern).expect_err(pattern)
    }

    fn search(pattern: &str, text: &str) -> bool {
        PyRegex::compile(pattern)
            .expect(pattern)
            .search(text)
            .expect(pattern)
    }

    #[test]
    fn errors_match_cpython() {
        let cases: &[(&str, &str, Option<usize>)] = &[
            ("(?<name>x)", "unknown extension ?<n", Some(1)),
            ("[", "unterminated character set", Some(0)),
            ("a**", "multiple repeat", Some(2)),
            (
                "a|(?i)b",
                "global flags not at the start of the expression",
                Some(2),
            ),
            (
                "(?P<a>x)(?P<a>y)",
                "redefinition of group name 'a' as group 2; was group 1",
                Some(12),
            ),
            ("(?<=a+)b", "look-behind requires fixed-width pattern", None),
            ("\\p{L}", "bad escape \\p", Some(0)),
            ("[z-a]", "bad character range z-a", Some(1)),
            ("(?P<1>x)", "bad character in group name '1'", Some(4)),
        ];
        for &(pattern, msg, pos) in cases {
            let e = err(pattern);
            assert_eq!(
                (e.kind, e.msg.as_str(), e.pos),
                (ErrorKind::Re, msg, pos),
                "{pattern}"
            );
            assert!(!is_valid(pattern));
        }
        assert_eq!(
            err("a\n(").python_str("a\n("),
            "missing ), unterminated subpattern at position 2 (line 2, column 1)"
        );
        assert_eq!(err("a{4294967295}").kind, ErrorKind::Overflow);
        assert_eq!(err("(?a)(?u)x").kind, ErrorKind::Value);
        assert_eq!(
            err(&format!("{}a{}", "(".repeat(486), ")".repeat(486))).kind,
            ErrorKind::Recursion
        );
        assert!(is_valid(&format!(
            "{}a{}",
            "(".repeat(485),
            ")".repeat(485)
        )));
        assert!(is_valid("\\N{EM DASH}"));
        assert!(is_valid("(?P<名前>x)(?P=名前)"));
    }

    #[test]
    fn search_matches_cpython() {
        let cases: &[(&str, &str, bool)] = &[
            ("a$", "a\n", true),
            ("a\\Z", "a\n", false),
            ("\\B", "", false),
            ("(?i)k", "\u{212a}", true),
            ("(?a)\\w", "é", false),
            ("(?a)(?u:\\w)", "é", false), // _compile_info charset-prefix quirk
            ("(a)?(?(1)b|c)", "c", true),
            ("\\d", "\u{663}", true),
            ("\\s", "\x1c", true),
            ("^[^/]+$", "a/b", false),
            ("(?x) a b # c", "ab", true),
            ("\\bfoo\\b", "foo²", false),
            ("\\bfoo\\b", "x foo.", true),
        ];
        for &(pattern, text, expected) in cases {
            assert_eq!(search(pattern, text), expected, "{pattern} on {text:?}");
        }
        // Large counted repeats run on the backtracking VM instead of failing.
        assert!(search("^\\w{1,1000}$", &"é".repeat(1000)));
    }

    #[test]
    fn unsupported_matching_is_an_error_not_a_wrong_answer() {
        for pattern in ["((?(1)a|b))", "\\N{EM DASH}"] {
            assert!(
                PyRegex::compile(pattern)
                    .expect(pattern)
                    .search("b")
                    .is_err()
            );
        }
        let deep = format!("{}a{}", "(".repeat(100), ")".repeat(100));
        assert!(PyRegex::compile(&deep).expect("valid").search("a").is_err());
    }

    #[test]
    fn python_3_14_syntax() {
        use crate::context::with_python_minor;
        // `\z` is a bad escape until 3.14, then the end of the string.
        assert_eq!(PyRegex::compile("x\\z").unwrap_err().msg, "bad escape \\z");
        with_python_minor(14, || {
            let re = PyRegex::compile("x\\z").expect("valid in 3.14");
            assert_eq!(re.search("x"), Ok(true));
            assert_eq!(re.search("x\n"), Ok(false));
            assert!(PyRegex::compile("[\\z]").is_err());
        });
        // `\B` matches the empty string from 3.14 on.
        assert!(!search("\\B", ""));
        with_python_minor(14, || {
            assert!(PyRegex::compile("\\B").unwrap().search("").unwrap());
            assert!(!PyRegex::compile("\\B").unwrap().search("a").unwrap());
        });
    }

    #[test]
    fn regex_is_shareable() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PyRegex>();
    }
}
