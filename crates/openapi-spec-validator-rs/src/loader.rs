//! Spec document loading, reproducing openapi-spec-validator's reader exactly.
//!
//! Upstream reads a spec file with `urlopen("file://…")`, so PyYAML gets a *binary* stream
//! named `"<urllib response>"`; stdin is `sys.stdin`, a text stream named `"<stdin>"`. Either
//! way the stream is parsed by PyYAML's `CSafeLoader` (libyaml 0.2.5 plus the Python
//! `SafeConstructor` and YAML 1.1 resolvers, with jsonschema-path's tweaks: no implicit
//! timestamps, an extra scientific-notation float rule) and the result goes through
//! `json.loads(json.dumps(data))`. The CLI prints `str(exc)` for any failure.
//!
//! The stages here mirror that pipeline:
//!
//! 1. `fast_json`: JSON that libyaml would scan the same way is parsed by serde_json.
//! 2. `libyaml`: the libyaml event parser (unsafe-libyaml is a faithful transpile), fed the
//!    same bytes in the same chunks as PyYAML's `CParser` (`text_input` emulates stdin).
//! 3. `compose`: `CParser`'s composer with PyYAML's implicit resolvers (`resolve`).
//! 4. `construct`: `SafeConstructor`, in its breadth-first order (`scalars` for `int()`,
//!    `float()`, base64 and timestamps).
//! 5. `dump`: the `json.dumps` traversal and errors, then `json.loads`' view of the result.
//!
//! # Known deviations
//!
//! * NaN and ±Infinity (`.nan`, `.inf`, `1e400`, ...) load as `Value::Null`: `serde_json::Value`
//!   cannot hold them, while Python keeps the float.
//! * Integers outside the 64-bit range become the `f64` that serde_json produces for the same
//!   decimal literal (as JSON input does); beyond `f64` range they become `Value::Null`.
//! * Error texts that embed `repr()` of user strings treat code points that are unassigned in
//!   Unicode 15.1 as printable, and `int()`/`float()` under explicit `!!int`/`!!float` tags do
//!   not accept non-ASCII decimal digits (Python does).
//! * PyYAML's own recursion limits (deep merge-key chains, `=`-key chains) are approximated.

mod bigint;
mod compose;
mod construct;
mod dump;
mod fast_json;
mod libyaml;
mod pyfmt;
mod resolve;
mod scalars;
mod text_input;

use serde_json::Value;

pub use text_input::TextErrors;

/// A read failure, carrying the text upstream's CLI prints (it prints `str(exc)` and exits 1).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct LoadError {
    pub message: String,
}

/// The deepest container nesting `json.dumps` accepts when upstream reads a file (Python's C
/// recursion budget minus what the CLI's call stack already uses).
const FILE_DEPTH_LIMIT: usize = 9989;
/// The same limit on the stdin path, whose call stack is a few frames shallower.
const STDIN_DEPTH_LIMIT: usize = 9992;

/// Parse a spec document exactly like openapi-spec-validator's file reader.
/// `stream_name` is the name PyYAML uses in error marks. For files upstream reads through
/// `urlopen`, so the name it prints is `"<urllib response>"`, never the path.
///
/// This models a binary stream: libyaml detects UTF-8/UTF-16 itself and reports bad bytes as
/// `ReaderError`s; `\r\n` is handled by libyaml. Use [`load_text`] for stdin.
///
/// Loading (and dropping) the deepest document accepted, about 9,990 nested containers,
/// takes under 1 MiB of stack in an optimized build.
pub fn load(bytes: &[u8], stream_name: &str) -> Result<Value, LoadError> {
    if let Some(value) = fast_json::try_load(bytes) {
        return Ok(value);
    }
    load_yaml(libyaml::Source::Bytes(bytes), stream_name, FILE_DEPTH_LIMIT)
}

/// Parse a document read from a text stream (upstream's stdin path: `sys.stdin`, normally
/// `stream_name = "<stdin>"`). `errors` is `sys.stdin.errors`, which upstream inherits from the
/// locale: [`TextErrors::Strict`] under UTF-8 locales, [`TextErrors::SurrogateEscape`] under
/// the C/POSIX locale. Only undecodable input behaves differently from [`load`].
pub fn load_text(bytes: &[u8], stream_name: &str, errors: TextErrors) -> Result<Value, LoadError> {
    // Valid JSON is valid UTF-8, and a text stream hands valid UTF-8 to libyaml unchanged.
    if let Some(value) = fast_json::try_load(bytes) {
        return Ok(value);
    }
    let feed = text_input::TextFeed::new(bytes, errors);
    load_yaml(libyaml::Source::Text(feed), stream_name, STDIN_DEPTH_LIMIT)
}

fn load_yaml(
    source: libyaml::Source<'_>,
    stream_name: &str,
    depth_limit: usize,
) -> Result<Value, LoadError> {
    let run = || -> Result<Value, String> {
        let mut parser = libyaml::Parser::new(source, stream_name);
        let mut doc = compose::compose(&mut parser, stream_name)?;
        // PyYAML has finished reading at this point; release the parser's buffers early.
        drop(parser);
        let (graph, root) = construct::construct(&mut doc, stream_name)?;
        let move_strings = !doc.has_aliases;
        dump::to_value(&mut doc.nodes, &graph, root, depth_limit, move_strings)
    };
    run().map_err(|message| LoadError { message })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn via_yaml(input: &str) -> Result<Value, LoadError> {
        load_yaml(
            libyaml::Source::Bytes(input.as_bytes()),
            "<test>",
            FILE_DEPTH_LIMIT,
        )
    }

    /// For JSON the fast path accepts, the YAML path must give the identical value.
    #[test]
    fn fast_path_agrees_with_yaml_path() {
        let docs = [
            r#"{"a": 1, "b": [true, false, null], "c": "x"}"#,
            r#"{"a": 1, "a": 2, "b": 3}"#,
            r#"[0, -0, -0.0, 0.0, 1e5, 1E5, 1.5e5, 1.5E-5, 0.1, 123456789.123456789]"#,
            r#"[18446744073709551615, 18446744073709551616, -9223372036854775808, -9223372036854775809]"#,
            r#"[1.7976931348623157e308, 2.2250738585072014e-308, 5e-324, 1e-400, 9007199254740993]"#,
            r#"{"esc": "~"~~~/~b~f~n~r~t~u0041~u00e9~u0000~u2028~u001f", "uni": "~u00e9 é😀日本"}"#,
            "{\n\t\"tab\":\t[1,\t2],\n  \"k\"  :  \"v\"\n}\n",
            "\"top\"",
            "  123  ",
            "-0",
            r#"{"<<": {"a": 1}, "=": "=", "null": null, "yes": "no"}"#,
            r#"[{"a": [{"b": {"c": []}}]}, {}]"#,
        ];
        for doc in docs {
            // `~` stands for a backslash in the literals above.
            let doc = &doc.replace('~', "\\");
            let fast = fast_json::try_load(doc.as_bytes());
            let slow = via_yaml(doc);
            assert!(fast.is_some(), "fast path declined {doc:?}");
            assert_eq!(fast.map(Ok), Some(slow), "{doc:?}");
        }
    }

    /// JSON that libyaml scans differently must not take the fast path.
    #[test]
    fn fast_path_declines_libyaml_incompatible_json() {
        let long_key = format!("{{\"{}\": 1}}", "k".repeat(1023));
        let docs = [
            r#""~ud83d~ude00""#.to_owned(),
            r#"{"a": "~uDFFF", "b": "~ud800"}"#.to_owned(),
            "\t{}".to_owned(),
            "{}\n\t".to_owned(),
            "{\"a\"\n: 1}".to_owned(),
            long_key,
            "\"a\u{7f}\"".to_owned(),
            "\"a\u{85}\"".to_owned(),
            "\"a\u{2028}\"".to_owned(),
            "\"\u{fffe}\"".to_owned(),
        ];
        for doc in &docs {
            let doc = &doc.replace('~', "\\");
            assert!(fast_json::try_load(doc.as_bytes()).is_none(), "{doc:?}");
            assert!(load(doc.as_bytes(), "<t>").is_ok() || via_yaml(doc).is_err());
        }
        let ok_key = format!("{{\"{}\": 1}}", "k".repeat(1022));
        assert!(fast_json::try_load(ok_key.as_bytes()).is_some());
    }
}
