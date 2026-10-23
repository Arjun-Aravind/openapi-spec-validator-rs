//! Python `repr()`, `pprint.pformat()` and jsonschema `ValidationError.__str__` for JSON values.
//!
//! Values are what `json.loads` produces (dict / list / str / int / float / bool / None).
//! Every length and width counts Unicode code points, like Python's `len()`. Mirrors
//! CPython 3.13 (`Objects/unicodeobject.c` `unicode_repr`, `Lib/pprint.py`, `Lib/textwrap.py`)
//! and jsonschema 4.26 (`exceptions._Error.__str__`, `_utils.format_as_index`).

use serde_json::{Map, Number, Value};
use std::fmt::Write as _;

// ---------------------------------------------------------------------------------------------
// repr
// ---------------------------------------------------------------------------------------------

/// `repr(obj)` for a `json.loads` value.
pub fn repr(value: &Value) -> String {
    let mut out = String::new();
    write_repr(value, false, &mut out);
    out
}

/// `repr(s)` for a `str`.
pub fn repr_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    write_repr_str(s, &mut out);
    out
}

/// `repr(f)` for a `float`: shortest round-trip digits, exponent form when the decimal
/// point position is `<= -4` or `> 16` (CPython `float_repr_style == 'short'`).
pub fn repr_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_owned();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.to_owned();
    }
    let sign = if f.is_sign_negative() { "-" } else { "" };
    if f == 0.0 {
        return format!("{sign}0.0");
    }
    // Rust's `{:e}` yields a shortest round-tripping digit string, e.g. "1.2345e-7", but when
    // several strings of that length round-trip it may not pick the one nearest to `f`, which
    // dtoa mode 0 does. The correctly rounded string of the same length is that nearest one.
    let shortest = format!("{:e}", f.abs());
    let ndigits = shortest
        .find('e')
        .map_or(1, |e| shortest[..e].replace('.', "").len());
    let nearest = format!("{:.*e}", ndigits - 1, f.abs());
    let sci = if nearest.parse::<f64>() == Ok(f.abs()) {
        nearest
    } else {
        shortest
    };
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let exp: i64 = exp.parse().unwrap_or(0);
    let decpt = exp + 1; // value == 0.DIGITS * 10**decpt
    let ndigits = digits.len() as i64;
    let mut out = String::from(sign);
    if decpt <= -4 || decpt > 16 {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let e = decpt - 1;
        let _ = write!(out, "e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
    } else if decpt <= 0 {
        out.push_str("0.");
        out.extend(std::iter::repeat_n('0', (-decpt) as usize));
        out.push_str(&digits);
    } else if decpt >= ndigits {
        out.push_str(&digits);
        out.extend(std::iter::repeat_n('0', (decpt - ndigits) as usize));
        out.push_str(".0");
    } else {
        out.push_str(&digits[..decpt as usize]);
        out.push('.');
        out.push_str(&digits[decpt as usize..]);
    }
    out
}

/// A JSON number as `json.loads` types it.
#[derive(Clone, Debug, PartialEq)]
pub enum PyNumber {
    Int(i128),
    /// An `int` beyond `i128` (decimal literal text); only with serde_json `arbitrary_precision`.
    BigInt(String),
    Float(f64),
}

/// Classifies a serde_json number like `json.loads` would. Without `arbitrary_precision`,
/// serde_json has already turned integers beyond 64 bits into floats and `-0` into `-0.0`.
pub fn py_number(n: &Number) -> PyNumber {
    if let Some(i) = n.as_i64() {
        PyNumber::Int(i128::from(i))
    } else if let Some(u) = n.as_u64() {
        PyNumber::Int(i128::from(u))
    } else if n.is_f64() {
        PyNumber::Float(n.as_f64().unwrap_or(f64::NAN))
    } else {
        // arbitrary_precision literal: a big integer, or a float literal that overflows
        // (json.loads gives `inf`, as does Rust's parser).
        let text = n.to_string();
        if text.contains(['.', 'e', 'E']) {
            PyNumber::Float(text.parse().unwrap_or(f64::NAN))
        } else if let Ok(i) = text.parse::<i128>() {
            PyNumber::Int(i)
        } else {
            PyNumber::BigInt(text)
        }
    }
}

/// `repr(int)` / `repr(float)` for a JSON number.
fn write_repr_number(n: &Number, out: &mut String) {
    match py_number(n) {
        PyNumber::Int(i) => {
            let _ = write!(out, "{i}");
        }
        PyNumber::BigInt(text) => out.push_str(&text),
        PyNumber::Float(f) => out.push_str(&repr_float(f)),
    }
}

/// `repr()`; with `sort_dicts` it is pprint's `_safe_repr` (dict items sorted by key).
fn write_repr(value: &Value, sort_dicts: bool, out: &mut String) {
    match value {
        Value::Null => out.push_str("None"),
        Value::Bool(true) => out.push_str("True"),
        Value::Bool(false) => out.push_str("False"),
        Value::Number(n) => write_repr_number(n, out),
        Value::String(s) => write_repr_str(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_repr(item, sort_dicts, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, v)) in dict_items(map, sort_dicts).into_iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_repr_str(k, out);
                out.push_str(": ");
                write_repr(v, sort_dicts, out);
            }
            out.push('}');
        }
    }
}

/// Dict items in insertion order, or sorted like `sorted(d.items(), key=_safe_tuple)`
/// (keys are unique `str`s, so this is code-point order of the keys).
fn dict_items(map: &Map<String, Value>, sort_dicts: bool) -> Vec<(&String, &Value)> {
    let mut items: Vec<_> = map.iter().collect();
    if sort_dicts {
        items.sort_unstable_by(|a, b| a.0.cmp(b.0));
    }
    items
}

/// Which quote `unicode_repr` picks: `"` only when the text has `'` but no `"`.
fn quote_for(s: &str) -> char {
    if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    }
}

fn write_repr_str(s: &str, out: &mut String) {
    let quote = quote_for(s);
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
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c if (c as u32) < 0x7f || py_isprintable(c) => out.push(c),
            c if (c as u32) <= 0xff => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c if (c as u32) <= 0xffff => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => {
                let _ = write!(out, "\\U{:08x}", c as u32);
            }
        }
    }
    out.push(quote);
}

/// Length of `repr(c)`'s escape for one char, ignoring quote escaping.
fn escaped_len(c: char) -> usize {
    match c {
        '\\' | '\t' | '\n' | '\r' => 2,
        c if (c as u32) < 0x20 || c as u32 == 0x7f => 4,
        c if (c as u32) < 0x7f || py_isprintable(c) => 1,
        c if (c as u32) <= 0xff => 4,
        c if (c as u32) <= 0xffff => 6,
        _ => 10,
    }
}

/// `len(repr(s)) > limit`, scanning only as far as needed to decide.
fn str_repr_exceeds(s: &str, limit: i64) -> bool {
    let mut remaining = limit;
    str_repr_consume(s, &mut remaining)
}

/// Subtracts `len(repr(s))` from `remaining`; returns true (early) once it goes negative.
fn str_repr_consume(s: &str, remaining: &mut i64) -> bool {
    *remaining -= 2;
    if *remaining < 0 {
        return true;
    }
    let (mut squotes, mut has_dquote) = (0i64, false);
    for c in s.chars() {
        *remaining -= escaped_len(c) as i64;
        if *remaining < 0 {
            return true;
        }
        match c {
            '\'' => squotes += 1,
            '"' => has_dquote = true,
            _ => {}
        }
    }
    // Both quote kinds present: `'` is used and every `'` gets a backslash.
    if has_dquote {
        *remaining -= squotes;
    }
    *remaining < 0
}

/// `len(repr(value)) > limit` (pprint's `_safe_repr`; item order does not change the length).
fn repr_exceeds(value: &Value, limit: i64) -> bool {
    let mut remaining = limit;
    repr_consume(value, &mut remaining)
}

fn repr_consume(value: &Value, remaining: &mut i64) -> bool {
    match value {
        Value::String(s) => return str_repr_consume(s, remaining),
        Value::Null | Value::Bool(_) | Value::Number(_) => {
            let mut rep = String::new();
            write_repr(value, false, &mut rep);
            *remaining -= rep.len() as i64; // ASCII
        }
        Value::Array(items) => {
            *remaining -= 2 + 2 * items.len().saturating_sub(1) as i64;
            for item in items {
                if *remaining < 0 || repr_consume(item, remaining) {
                    return true;
                }
            }
        }
        Value::Object(map) => {
            // "{" + "k: v" joined by ", " + "}"
            *remaining -= 2 + 2 * map.len() as i64 + 2 * map.len().saturating_sub(1) as i64;
            for (k, v) in map {
                if *remaining < 0 || str_repr_consume(k, remaining) || repr_consume(v, remaining) {
                    return true;
                }
            }
        }
    }
    *remaining < 0
}

fn char_len(s: &str) -> i64 {
    s.chars().count() as i64
}

// ---------------------------------------------------------------------------------------------
// pprint
// ---------------------------------------------------------------------------------------------

/// `pprint.pformat(obj, width=width, sort_dicts=sort_dicts)` (indent=1, depth=None, compact=False).
///
/// Python rejects `width=0`; here it simply formats with that width.
pub fn pformat(value: &Value, width: usize, sort_dicts: bool) -> String {
    let mut out = String::new();
    pformat_into(value, width, sort_dicts, 0, &mut out);
    out
}

/// `pformat` appended to `out`, with `line_prefix` extra spaces after every newline
/// (`textwrap.indent` of the result, minus the first line).
fn pformat_into(
    value: &Value,
    width: usize,
    sort_dicts: bool,
    line_prefix: usize,
    out: &mut String,
) {
    let mut printer = PrettyPrinter {
        width: i64::try_from(width).unwrap_or(i64::MAX / 4),
        sort_dicts,
        line_prefix,
        out,
    };
    printer.format(value, 0, 0, 0);
}

struct PrettyPrinter<'a> {
    width: i64,
    sort_dicts: bool,
    line_prefix: usize,
    out: &'a mut String,
}

impl PrettyPrinter<'_> {
    /// `PrettyPrinter._format`: use the one-line repr if it fits, else the type's `_pprint_*`.
    fn format(&mut self, value: &Value, indent: i64, allowance: i64, level: usize) {
        let max_width = self.width - indent - allowance;
        match value {
            Value::Object(map) if repr_exceeds(value, max_width) => {
                self.pprint_dict(map, indent, allowance, level + 1)
            }
            Value::Array(items) if repr_exceeds(value, max_width) => {
                self.pprint_list(items, indent, allowance, level + 1)
            }
            Value::String(s) if str_repr_exceeds(s, max_width) => {
                self.pprint_str(s, indent, allowance, level + 1)
            }
            // Scalars have no `_dispatch` entry: always written whole.
            _ => write_repr(value, self.sort_dicts, self.out),
        }
    }

    fn newline_indent(&mut self, indent: i64) {
        self.out.push('\n');
        let spaces = self.line_prefix + indent.max(0) as usize;
        self.out.extend(std::iter::repeat_n(' ', spaces));
    }

    fn pprint_dict(&mut self, map: &Map<String, Value>, indent: i64, allowance: i64, level: usize) {
        self.out.push('{');
        if !map.is_empty() {
            let items = dict_items(map, self.sort_dicts);
            self.format_dict_items(&items, indent, allowance + 1, level);
        }
        self.out.push('}');
    }

    fn format_dict_items(
        &mut self,
        items: &[(&String, &Value)],
        indent: i64,
        allowance: i64,
        level: usize,
    ) {
        let indent = indent + 1;
        let last_index = items.len() - 1;
        for (i, (key, ent)) in items.iter().enumerate() {
            let last = i == last_index;
            let rep = repr_str(key);
            self.out.push_str(&rep);
            self.out.push_str(": ");
            let child_indent = indent + char_len(&rep) + 2;
            self.format(ent, child_indent, if last { allowance } else { 1 }, level);
            if !last {
                self.out.push(',');
                self.newline_indent(indent);
            }
        }
    }

    fn pprint_list(&mut self, items: &[Value], indent: i64, allowance: i64, level: usize) {
        self.out.push('[');
        // `_format_items` (compact=False)
        let indent = indent + 1;
        let allowance = allowance + 1;
        for (i, ent) in items.iter().enumerate() {
            let last = i + 1 == items.len();
            if i > 0 {
                self.out.push(',');
                self.newline_indent(indent);
            }
            self.format(ent, indent, if last { allowance } else { 1 }, level);
        }
        self.out.push(']');
    }

    /// `_pprint_str`: split per line, then on whitespace runs, into implicitly
    /// concatenated literals (parenthesised only at the top level).
    fn pprint_str(&mut self, s: &str, mut indent: i64, mut allowance: i64, level: usize) {
        if s.is_empty() {
            self.out.push_str("''");
            return;
        }
        let lines = splitlines_keepends(s);
        if level == 1 {
            indent += 1;
            allowance += 1;
        }
        let max_width = self.width - indent;
        let mut max_width1 = max_width;
        let mut chunks: Vec<String> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let last_line = i + 1 == lines.len();
            if last_line {
                max_width1 -= allowance;
            }
            if !str_repr_exceeds(line, max_width1) {
                chunks.push(repr_str(line));
                continue;
            }
            // `re.findall(r'\S*\s*', line)` minus the trailing empty match.
            let parts = space_parts(line);
            let mut max_width2 = max_width;
            // `current` is always the byte range line[cur_start..part_start].
            let mut cur_start = 0;
            let mut part_start = 0;
            for (j, part) in parts.iter().enumerate() {
                let part_end = part_start + part.len();
                if last_line && j + 1 == parts.len() {
                    max_width2 -= allowance;
                }
                if str_repr_exceeds(&line[cur_start..part_end], max_width2) {
                    if cur_start < part_start {
                        chunks.push(repr_str(&line[cur_start..part_start]));
                    }
                    cur_start = part_start;
                }
                part_start = part_end;
            }
            if cur_start < part_start {
                chunks.push(repr_str(&line[cur_start..part_start]));
            }
        }
        if chunks.len() == 1 {
            self.out.push_str(&chunks[0]);
            return;
        }
        if level == 1 {
            self.out.push('(');
        }
        for (i, rep) in chunks.iter().enumerate() {
            if i > 0 {
                self.newline_indent(indent);
            }
            self.out.push_str(rep);
        }
        if level == 1 {
            self.out.push(')');
        }
    }
}

/// `str.splitlines(keepends=True)`.
fn splitlines_keepends(s: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if is_line_break(c) {
            let mut end = i + c.len_utf8();
            if c == '\r' && matches!(chars.peek(), Some((_, '\n'))) {
                chars.next();
                end += 1;
            }
            lines.push(&s[start..end]);
            start = end;
        }
    }
    if start < s.len() {
        lines.push(&s[start..]);
    }
    lines
}

fn is_line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r'
            | '\x0b'
            | '\x0c'
            | '\x1c'
            | '\x1d'
            | '\x1e'
            | '\u{85}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

/// Maximal `\S*\s*` runs of `line` (Python `re` `\s` == `str.isspace`).
fn space_parts(line: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut in_space = false;
    for (i, c) in line.char_indices() {
        let space = py_isspace(c);
        if !space && in_space {
            parts.push(&line[start..i]);
            start = i;
        }
        in_space = space;
    }
    if start < line.len() {
        parts.push(&line[start..]);
    }
    parts
}

/// `str.isspace()` for one char (CPython `Py_UNICODE_ISSPACE`, Unicode 15.1).
pub fn py_isspace(c: char) -> bool {
    matches!(
        c,
        '\t'..='\r'
            | '\x1c'..='\x1f'
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

/// `str.isprintable()` for one char (CPython `Py_UNICODE_ISPRINTABLE`, Unicode 15.1).
pub fn py_isprintable(c: char) -> bool {
    let cp = c as u32;
    // Index of the first range whose end is >= cp.
    let ranges = NON_PRINTABLE.len() / 2;
    let (mut lo, mut hi) = (0, ranges);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if NON_PRINTABLE[2 * mid + 1] < cp {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    !(lo < ranges && NON_PRINTABLE[2 * lo] <= cp)
}

// ---------------------------------------------------------------------------------------------
// jsonschema error rendering
// ---------------------------------------------------------------------------------------------

/// One element of a jsonschema `path` / `schema_path` deque.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathItem {
    Key(String),
    Index(usize),
}

/// jsonschema `_utils.format_as_index(container, indices)`, e.g. `schema['properties']['a']`.
pub fn format_as_index(container: &str, indices: &[PathItem]) -> String {
    let mut out = String::from(container);
    if indices.is_empty() {
        return out;
    }
    out.push('[');
    for (i, index) in indices.iter().enumerate() {
        if i > 0 {
            out.push_str("][");
        }
        match index {
            PathItem::Key(k) => write_repr_str(k, &mut out),
            PathItem::Index(n) => {
                let _ = write!(out, "{n}");
            }
        }
    }
    out.push(']');
    out
}

/// Which jsonschema error class is rendered: it picks the words used for the two paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// `ValidationError`: "schema" / "instance"
    Validation,
    /// `SchemaError`: "metaschema" / "schema"
    Schema,
}

/// `str(error)` for a jsonschema 4.26 `ValidationError` with every verbose field set.
///
/// `schema_path` is the error's full `relative_schema_path` (its last element, the failing
/// keyword, is dropped as upstream does); `instance_path` is `relative_path`.
pub fn format_validation_error(
    message: &str,
    validator: &str,
    schema_path: &[PathItem],
    schema: &Value,
    instance_path: &[PathItem],
    instance: &Value,
) -> String {
    format_jsonschema_error(
        ErrorKind::Validation,
        message,
        Some(validator),
        schema_path,
        schema,
        instance_path,
        instance,
    )
}

/// General form of [`format_validation_error`]: `validator` is `None` for errors raised by a
/// `false` schema (rendered as `Failed validating None in ...`).
pub fn format_jsonschema_error(
    kind: ErrorKind,
    message: &str,
    validator: Option<&str>,
    schema_path: &[PathItem],
    schema: &Value,
    instance_path: &[PathItem],
    instance: &Value,
) -> String {
    let (schema_word, instance_word) = match kind {
        ErrorKind::Validation => ("schema", "instance"),
        ErrorKind::Schema => ("metaschema", "schema"),
    };
    let schema_path = format_as_index(
        schema_word,
        &schema_path[..schema_path.len().saturating_sub(1)],
    );
    let instance_path = format_as_index(instance_word, instance_path);
    let validator = validator.map_or_else(|| "None".to_owned(), repr_str);

    // Upstream renders `dedent(f"""...""".rstrip())` where every template line carries the
    // source's 12-space indentation:
    //
    //     {message}
    //     <blank>
    //     Failed validating {validator!r} in {schema_path}:
    //         {_pretty(schema)}        <- indent(pformat(...), 16 * " ").lstrip()
    //     <blank>
    //     On {instance_path}:
    //         {_pretty(instance)}
    //
    // The pformat blocks are streamed straight into the output: their lines start with at least
    // 16 spaces (deeper than any margin, which is at most the fixed lines' 12), pformat output
    // breaks lines only with '\n' (repr escapes every other line boundary) and has no blank or
    // trailing-whitespace lines, so `indent`, `rstrip` and `dedent` reduce to "16 spaces minus
    // the margin after each newline". Only the message lines need the general treatment.
    const PAD: &str = "            ";
    let head = format!("{PAD}{message}");
    let failed = format!("{PAD}Failed validating {validator} in {schema_path}:");
    let on = format!("{PAD}On {instance_path}:");
    // dedent step 1: lines made only of spaces/tabs become empty.
    let head_lines: Vec<&str> = head
        .split('\n')
        .map(|line| {
            if line.chars().all(|c| c == ' ' || c == '\t') {
                ""
            } else {
                line
            }
        })
        .collect();
    let margin = dedent_margin(
        head_lines
            .iter()
            .copied()
            .chain([failed.as_str(), on.as_str()]),
    );
    // margin is at most the 12 spaces of the fixed lines.
    let block_indent = 16usize.saturating_sub(margin.len());

    let mut out = String::new();
    for (i, line) in head_lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line.strip_prefix(margin).unwrap_or(line));
    }
    for (label, value) in [(&failed, schema), (&on, instance)] {
        out.push_str("\n\n");
        out.push_str(label.strip_prefix(margin).unwrap_or(label));
        out.push('\n');
        out.extend(std::iter::repeat_n(' ', block_indent));
        pformat_into(value, 72, false, block_indent, &mut out);
    }
    out
}

/// `textwrap.dedent`'s margin: the longest `[ \t]*` prefix common to every line that has
/// content (whitespace-only lines must already be blanked).
fn dedent_margin<'a>(lines: impl Iterator<Item = &'a str>) -> &'a str {
    let mut margin: Option<&str> = None;
    for line in lines {
        let indent_len = line.len() - line.trim_start_matches([' ', '\t']).len();
        if indent_len == line.len() {
            continue; // blank
        }
        let indent = &line[..indent_len];
        margin = Some(match margin {
            None => indent,
            Some(m) if indent.starts_with(m) => m,
            Some(m) if m.starts_with(indent) => indent,
            Some(m) => {
                let common = m
                    .bytes()
                    .zip(indent.bytes())
                    .take_while(|(a, b)| a == b)
                    .count();
                &m[..common]
            }
        });
    }
    margin.unwrap_or("")
}

// ---------------------------------------------------------------------------------------------
// Unicode data (generated by gen/gen_tables.py from CPython 3.13.14)
// ---------------------------------------------------------------------------------------------

/// Flattened `[start, end]` ranges of chars with `not str.isprintable()` (CPython 3.13, Unicode 15.1.0).
static NON_PRINTABLE: &[u32] = &[
    0x0, 0x1f, 0x7f, 0xa0, 0xad, 0xad, 0x378, 0x379, 0x380, 0x383, 0x38b, 0x38b, 0x38d, 0x38d,
    0x3a2, 0x3a2, 0x530, 0x530, 0x557, 0x558, 0x58b, 0x58c, 0x590, 0x590, 0x5c8, 0x5cf, 0x5eb,
    0x5ee, 0x5f5, 0x605, 0x61c, 0x61c, 0x6dd, 0x6dd, 0x70e, 0x70f, 0x74b, 0x74c, 0x7b2, 0x7bf,
    0x7fb, 0x7fc, 0x82e, 0x82f, 0x83f, 0x83f, 0x85c, 0x85d, 0x85f, 0x85f, 0x86b, 0x86f, 0x88f,
    0x897, 0x8e2, 0x8e2, 0x984, 0x984, 0x98d, 0x98e, 0x991, 0x992, 0x9a9, 0x9a9, 0x9b1, 0x9b1,
    0x9b3, 0x9b5, 0x9ba, 0x9bb, 0x9c5, 0x9c6, 0x9c9, 0x9ca, 0x9cf, 0x9d6, 0x9d8, 0x9db, 0x9de,
    0x9de, 0x9e4, 0x9e5, 0x9ff, 0xa00, 0xa04, 0xa04, 0xa0b, 0xa0e, 0xa11, 0xa12, 0xa29, 0xa29,
    0xa31, 0xa31, 0xa34, 0xa34, 0xa37, 0xa37, 0xa3a, 0xa3b, 0xa3d, 0xa3d, 0xa43, 0xa46, 0xa49,
    0xa4a, 0xa4e, 0xa50, 0xa52, 0xa58, 0xa5d, 0xa5d, 0xa5f, 0xa65, 0xa77, 0xa80, 0xa84, 0xa84,
    0xa8e, 0xa8e, 0xa92, 0xa92, 0xaa9, 0xaa9, 0xab1, 0xab1, 0xab4, 0xab4, 0xaba, 0xabb, 0xac6,
    0xac6, 0xaca, 0xaca, 0xace, 0xacf, 0xad1, 0xadf, 0xae4, 0xae5, 0xaf2, 0xaf8, 0xb00, 0xb00,
    0xb04, 0xb04, 0xb0d, 0xb0e, 0xb11, 0xb12, 0xb29, 0xb29, 0xb31, 0xb31, 0xb34, 0xb34, 0xb3a,
    0xb3b, 0xb45, 0xb46, 0xb49, 0xb4a, 0xb4e, 0xb54, 0xb58, 0xb5b, 0xb5e, 0xb5e, 0xb64, 0xb65,
    0xb78, 0xb81, 0xb84, 0xb84, 0xb8b, 0xb8d, 0xb91, 0xb91, 0xb96, 0xb98, 0xb9b, 0xb9b, 0xb9d,
    0xb9d, 0xba0, 0xba2, 0xba5, 0xba7, 0xbab, 0xbad, 0xbba, 0xbbd, 0xbc3, 0xbc5, 0xbc9, 0xbc9,
    0xbce, 0xbcf, 0xbd1, 0xbd6, 0xbd8, 0xbe5, 0xbfb, 0xbff, 0xc0d, 0xc0d, 0xc11, 0xc11, 0xc29,
    0xc29, 0xc3a, 0xc3b, 0xc45, 0xc45, 0xc49, 0xc49, 0xc4e, 0xc54, 0xc57, 0xc57, 0xc5b, 0xc5c,
    0xc5e, 0xc5f, 0xc64, 0xc65, 0xc70, 0xc76, 0xc8d, 0xc8d, 0xc91, 0xc91, 0xca9, 0xca9, 0xcb4,
    0xcb4, 0xcba, 0xcbb, 0xcc5, 0xcc5, 0xcc9, 0xcc9, 0xcce, 0xcd4, 0xcd7, 0xcdc, 0xcdf, 0xcdf,
    0xce4, 0xce5, 0xcf0, 0xcf0, 0xcf4, 0xcff, 0xd0d, 0xd0d, 0xd11, 0xd11, 0xd45, 0xd45, 0xd49,
    0xd49, 0xd50, 0xd53, 0xd64, 0xd65, 0xd80, 0xd80, 0xd84, 0xd84, 0xd97, 0xd99, 0xdb2, 0xdb2,
    0xdbc, 0xdbc, 0xdbe, 0xdbf, 0xdc7, 0xdc9, 0xdcb, 0xdce, 0xdd5, 0xdd5, 0xdd7, 0xdd7, 0xde0,
    0xde5, 0xdf0, 0xdf1, 0xdf5, 0xe00, 0xe3b, 0xe3e, 0xe5c, 0xe80, 0xe83, 0xe83, 0xe85, 0xe85,
    0xe8b, 0xe8b, 0xea4, 0xea4, 0xea6, 0xea6, 0xebe, 0xebf, 0xec5, 0xec5, 0xec7, 0xec7, 0xecf,
    0xecf, 0xeda, 0xedb, 0xee0, 0xeff, 0xf48, 0xf48, 0xf6d, 0xf70, 0xf98, 0xf98, 0xfbd, 0xfbd,
    0xfcd, 0xfcd, 0xfdb, 0xfff, 0x10c6, 0x10c6, 0x10c8, 0x10cc, 0x10ce, 0x10cf, 0x1249, 0x1249,
    0x124e, 0x124f, 0x1257, 0x1257, 0x1259, 0x1259, 0x125e, 0x125f, 0x1289, 0x1289, 0x128e, 0x128f,
    0x12b1, 0x12b1, 0x12b6, 0x12b7, 0x12bf, 0x12bf, 0x12c1, 0x12c1, 0x12c6, 0x12c7, 0x12d7, 0x12d7,
    0x1311, 0x1311, 0x1316, 0x1317, 0x135b, 0x135c, 0x137d, 0x137f, 0x139a, 0x139f, 0x13f6, 0x13f7,
    0x13fe, 0x13ff, 0x1680, 0x1680, 0x169d, 0x169f, 0x16f9, 0x16ff, 0x1716, 0x171e, 0x1737, 0x173f,
    0x1754, 0x175f, 0x176d, 0x176d, 0x1771, 0x1771, 0x1774, 0x177f, 0x17de, 0x17df, 0x17ea, 0x17ef,
    0x17fa, 0x17ff, 0x180e, 0x180e, 0x181a, 0x181f, 0x1879, 0x187f, 0x18ab, 0x18af, 0x18f6, 0x18ff,
    0x191f, 0x191f, 0x192c, 0x192f, 0x193c, 0x193f, 0x1941, 0x1943, 0x196e, 0x196f, 0x1975, 0x197f,
    0x19ac, 0x19af, 0x19ca, 0x19cf, 0x19db, 0x19dd, 0x1a1c, 0x1a1d, 0x1a5f, 0x1a5f, 0x1a7d, 0x1a7e,
    0x1a8a, 0x1a8f, 0x1a9a, 0x1a9f, 0x1aae, 0x1aaf, 0x1acf, 0x1aff, 0x1b4d, 0x1b4f, 0x1b7f, 0x1b7f,
    0x1bf4, 0x1bfb, 0x1c38, 0x1c3a, 0x1c4a, 0x1c4c, 0x1c89, 0x1c8f, 0x1cbb, 0x1cbc, 0x1cc8, 0x1ccf,
    0x1cfb, 0x1cff, 0x1f16, 0x1f17, 0x1f1e, 0x1f1f, 0x1f46, 0x1f47, 0x1f4e, 0x1f4f, 0x1f58, 0x1f58,
    0x1f5a, 0x1f5a, 0x1f5c, 0x1f5c, 0x1f5e, 0x1f5e, 0x1f7e, 0x1f7f, 0x1fb5, 0x1fb5, 0x1fc5, 0x1fc5,
    0x1fd4, 0x1fd5, 0x1fdc, 0x1fdc, 0x1ff0, 0x1ff1, 0x1ff5, 0x1ff5, 0x1fff, 0x200f, 0x2028, 0x202f,
    0x205f, 0x206f, 0x2072, 0x2073, 0x208f, 0x208f, 0x209d, 0x209f, 0x20c1, 0x20cf, 0x20f1, 0x20ff,
    0x218c, 0x218f, 0x2427, 0x243f, 0x244b, 0x245f, 0x2b74, 0x2b75, 0x2b96, 0x2b96, 0x2cf4, 0x2cf8,
    0x2d26, 0x2d26, 0x2d28, 0x2d2c, 0x2d2e, 0x2d2f, 0x2d68, 0x2d6e, 0x2d71, 0x2d7e, 0x2d97, 0x2d9f,
    0x2da7, 0x2da7, 0x2daf, 0x2daf, 0x2db7, 0x2db7, 0x2dbf, 0x2dbf, 0x2dc7, 0x2dc7, 0x2dcf, 0x2dcf,
    0x2dd7, 0x2dd7, 0x2ddf, 0x2ddf, 0x2e5e, 0x2e7f, 0x2e9a, 0x2e9a, 0x2ef4, 0x2eff, 0x2fd6, 0x2fef,
    0x3000, 0x3000, 0x3040, 0x3040, 0x3097, 0x3098, 0x3100, 0x3104, 0x3130, 0x3130, 0x318f, 0x318f,
    0x31e4, 0x31ee, 0x321f, 0x321f, 0xa48d, 0xa48f, 0xa4c7, 0xa4cf, 0xa62c, 0xa63f, 0xa6f8, 0xa6ff,
    0xa7cb, 0xa7cf, 0xa7d2, 0xa7d2, 0xa7d4, 0xa7d4, 0xa7da, 0xa7f1, 0xa82d, 0xa82f, 0xa83a, 0xa83f,
    0xa878, 0xa87f, 0xa8c6, 0xa8cd, 0xa8da, 0xa8df, 0xa954, 0xa95e, 0xa97d, 0xa97f, 0xa9ce, 0xa9ce,
    0xa9da, 0xa9dd, 0xa9ff, 0xa9ff, 0xaa37, 0xaa3f, 0xaa4e, 0xaa4f, 0xaa5a, 0xaa5b, 0xaac3, 0xaada,
    0xaaf7, 0xab00, 0xab07, 0xab08, 0xab0f, 0xab10, 0xab17, 0xab1f, 0xab27, 0xab27, 0xab2f, 0xab2f,
    0xab6c, 0xab6f, 0xabee, 0xabef, 0xabfa, 0xabff, 0xd7a4, 0xd7af, 0xd7c7, 0xd7ca, 0xd7fc, 0xf8ff,
    0xfa6e, 0xfa6f, 0xfada, 0xfaff, 0xfb07, 0xfb12, 0xfb18, 0xfb1c, 0xfb37, 0xfb37, 0xfb3d, 0xfb3d,
    0xfb3f, 0xfb3f, 0xfb42, 0xfb42, 0xfb45, 0xfb45, 0xfbc3, 0xfbd2, 0xfd90, 0xfd91, 0xfdc8, 0xfdce,
    0xfdd0, 0xfdef, 0xfe1a, 0xfe1f, 0xfe53, 0xfe53, 0xfe67, 0xfe67, 0xfe6c, 0xfe6f, 0xfe75, 0xfe75,
    0xfefd, 0xff00, 0xffbf, 0xffc1, 0xffc8, 0xffc9, 0xffd0, 0xffd1, 0xffd8, 0xffd9, 0xffdd, 0xffdf,
    0xffe7, 0xffe7, 0xffef, 0xfffb, 0xfffe, 0xffff, 0x1000c, 0x1000c, 0x10027, 0x10027, 0x1003b,
    0x1003b, 0x1003e, 0x1003e, 0x1004e, 0x1004f, 0x1005e, 0x1007f, 0x100fb, 0x100ff, 0x10103,
    0x10106, 0x10134, 0x10136, 0x1018f, 0x1018f, 0x1019d, 0x1019f, 0x101a1, 0x101cf, 0x101fe,
    0x1027f, 0x1029d, 0x1029f, 0x102d1, 0x102df, 0x102fc, 0x102ff, 0x10324, 0x1032c, 0x1034b,
    0x1034f, 0x1037b, 0x1037f, 0x1039e, 0x1039e, 0x103c4, 0x103c7, 0x103d6, 0x103ff, 0x1049e,
    0x1049f, 0x104aa, 0x104af, 0x104d4, 0x104d7, 0x104fc, 0x104ff, 0x10528, 0x1052f, 0x10564,
    0x1056e, 0x1057b, 0x1057b, 0x1058b, 0x1058b, 0x10593, 0x10593, 0x10596, 0x10596, 0x105a2,
    0x105a2, 0x105b2, 0x105b2, 0x105ba, 0x105ba, 0x105bd, 0x105ff, 0x10737, 0x1073f, 0x10756,
    0x1075f, 0x10768, 0x1077f, 0x10786, 0x10786, 0x107b1, 0x107b1, 0x107bb, 0x107ff, 0x10806,
    0x10807, 0x10809, 0x10809, 0x10836, 0x10836, 0x10839, 0x1083b, 0x1083d, 0x1083e, 0x10856,
    0x10856, 0x1089f, 0x108a6, 0x108b0, 0x108df, 0x108f3, 0x108f3, 0x108f6, 0x108fa, 0x1091c,
    0x1091e, 0x1093a, 0x1093e, 0x10940, 0x1097f, 0x109b8, 0x109bb, 0x109d0, 0x109d1, 0x10a04,
    0x10a04, 0x10a07, 0x10a0b, 0x10a14, 0x10a14, 0x10a18, 0x10a18, 0x10a36, 0x10a37, 0x10a3b,
    0x10a3e, 0x10a49, 0x10a4f, 0x10a59, 0x10a5f, 0x10aa0, 0x10abf, 0x10ae7, 0x10aea, 0x10af7,
    0x10aff, 0x10b36, 0x10b38, 0x10b56, 0x10b57, 0x10b73, 0x10b77, 0x10b92, 0x10b98, 0x10b9d,
    0x10ba8, 0x10bb0, 0x10bff, 0x10c49, 0x10c7f, 0x10cb3, 0x10cbf, 0x10cf3, 0x10cf9, 0x10d28,
    0x10d2f, 0x10d3a, 0x10e5f, 0x10e7f, 0x10e7f, 0x10eaa, 0x10eaa, 0x10eae, 0x10eaf, 0x10eb2,
    0x10efc, 0x10f28, 0x10f2f, 0x10f5a, 0x10f6f, 0x10f8a, 0x10faf, 0x10fcc, 0x10fdf, 0x10ff7,
    0x10fff, 0x1104e, 0x11051, 0x11076, 0x1107e, 0x110bd, 0x110bd, 0x110c3, 0x110cf, 0x110e9,
    0x110ef, 0x110fa, 0x110ff, 0x11135, 0x11135, 0x11148, 0x1114f, 0x11177, 0x1117f, 0x111e0,
    0x111e0, 0x111f5, 0x111ff, 0x11212, 0x11212, 0x11242, 0x1127f, 0x11287, 0x11287, 0x11289,
    0x11289, 0x1128e, 0x1128e, 0x1129e, 0x1129e, 0x112aa, 0x112af, 0x112eb, 0x112ef, 0x112fa,
    0x112ff, 0x11304, 0x11304, 0x1130d, 0x1130e, 0x11311, 0x11312, 0x11329, 0x11329, 0x11331,
    0x11331, 0x11334, 0x11334, 0x1133a, 0x1133a, 0x11345, 0x11346, 0x11349, 0x1134a, 0x1134e,
    0x1134f, 0x11351, 0x11356, 0x11358, 0x1135c, 0x11364, 0x11365, 0x1136d, 0x1136f, 0x11375,
    0x113ff, 0x1145c, 0x1145c, 0x11462, 0x1147f, 0x114c8, 0x114cf, 0x114da, 0x1157f, 0x115b6,
    0x115b7, 0x115de, 0x115ff, 0x11645, 0x1164f, 0x1165a, 0x1165f, 0x1166d, 0x1167f, 0x116ba,
    0x116bf, 0x116ca, 0x116ff, 0x1171b, 0x1171c, 0x1172c, 0x1172f, 0x11747, 0x117ff, 0x1183c,
    0x1189f, 0x118f3, 0x118fe, 0x11907, 0x11908, 0x1190a, 0x1190b, 0x11914, 0x11914, 0x11917,
    0x11917, 0x11936, 0x11936, 0x11939, 0x1193a, 0x11947, 0x1194f, 0x1195a, 0x1199f, 0x119a8,
    0x119a9, 0x119d8, 0x119d9, 0x119e5, 0x119ff, 0x11a48, 0x11a4f, 0x11aa3, 0x11aaf, 0x11af9,
    0x11aff, 0x11b0a, 0x11bff, 0x11c09, 0x11c09, 0x11c37, 0x11c37, 0x11c46, 0x11c4f, 0x11c6d,
    0x11c6f, 0x11c90, 0x11c91, 0x11ca8, 0x11ca8, 0x11cb7, 0x11cff, 0x11d07, 0x11d07, 0x11d0a,
    0x11d0a, 0x11d37, 0x11d39, 0x11d3b, 0x11d3b, 0x11d3e, 0x11d3e, 0x11d48, 0x11d4f, 0x11d5a,
    0x11d5f, 0x11d66, 0x11d66, 0x11d69, 0x11d69, 0x11d8f, 0x11d8f, 0x11d92, 0x11d92, 0x11d99,
    0x11d9f, 0x11daa, 0x11edf, 0x11ef9, 0x11eff, 0x11f11, 0x11f11, 0x11f3b, 0x11f3d, 0x11f5a,
    0x11faf, 0x11fb1, 0x11fbf, 0x11ff2, 0x11ffe, 0x1239a, 0x123ff, 0x1246f, 0x1246f, 0x12475,
    0x1247f, 0x12544, 0x12f8f, 0x12ff3, 0x12fff, 0x13430, 0x1343f, 0x13456, 0x143ff, 0x14647,
    0x167ff, 0x16a39, 0x16a3f, 0x16a5f, 0x16a5f, 0x16a6a, 0x16a6d, 0x16abf, 0x16abf, 0x16aca,
    0x16acf, 0x16aee, 0x16aef, 0x16af6, 0x16aff, 0x16b46, 0x16b4f, 0x16b5a, 0x16b5a, 0x16b62,
    0x16b62, 0x16b78, 0x16b7c, 0x16b90, 0x16e3f, 0x16e9b, 0x16eff, 0x16f4b, 0x16f4e, 0x16f88,
    0x16f8e, 0x16fa0, 0x16fdf, 0x16fe5, 0x16fef, 0x16ff2, 0x16fff, 0x187f8, 0x187ff, 0x18cd6,
    0x18cff, 0x18d09, 0x1afef, 0x1aff4, 0x1aff4, 0x1affc, 0x1affc, 0x1afff, 0x1afff, 0x1b123,
    0x1b131, 0x1b133, 0x1b14f, 0x1b153, 0x1b154, 0x1b156, 0x1b163, 0x1b168, 0x1b16f, 0x1b2fc,
    0x1bbff, 0x1bc6b, 0x1bc6f, 0x1bc7d, 0x1bc7f, 0x1bc89, 0x1bc8f, 0x1bc9a, 0x1bc9b, 0x1bca0,
    0x1ceff, 0x1cf2e, 0x1cf2f, 0x1cf47, 0x1cf4f, 0x1cfc4, 0x1cfff, 0x1d0f6, 0x1d0ff, 0x1d127,
    0x1d128, 0x1d173, 0x1d17a, 0x1d1eb, 0x1d1ff, 0x1d246, 0x1d2bf, 0x1d2d4, 0x1d2df, 0x1d2f4,
    0x1d2ff, 0x1d357, 0x1d35f, 0x1d379, 0x1d3ff, 0x1d455, 0x1d455, 0x1d49d, 0x1d49d, 0x1d4a0,
    0x1d4a1, 0x1d4a3, 0x1d4a4, 0x1d4a7, 0x1d4a8, 0x1d4ad, 0x1d4ad, 0x1d4ba, 0x1d4ba, 0x1d4bc,
    0x1d4bc, 0x1d4c4, 0x1d4c4, 0x1d506, 0x1d506, 0x1d50b, 0x1d50c, 0x1d515, 0x1d515, 0x1d51d,
    0x1d51d, 0x1d53a, 0x1d53a, 0x1d53f, 0x1d53f, 0x1d545, 0x1d545, 0x1d547, 0x1d549, 0x1d551,
    0x1d551, 0x1d6a6, 0x1d6a7, 0x1d7cc, 0x1d7cd, 0x1da8c, 0x1da9a, 0x1daa0, 0x1daa0, 0x1dab0,
    0x1deff, 0x1df1f, 0x1df24, 0x1df2b, 0x1dfff, 0x1e007, 0x1e007, 0x1e019, 0x1e01a, 0x1e022,
    0x1e022, 0x1e025, 0x1e025, 0x1e02b, 0x1e02f, 0x1e06e, 0x1e08e, 0x1e090, 0x1e0ff, 0x1e12d,
    0x1e12f, 0x1e13e, 0x1e13f, 0x1e14a, 0x1e14d, 0x1e150, 0x1e28f, 0x1e2af, 0x1e2bf, 0x1e2fa,
    0x1e2fe, 0x1e300, 0x1e4cf, 0x1e4fa, 0x1e7df, 0x1e7e7, 0x1e7e7, 0x1e7ec, 0x1e7ec, 0x1e7ef,
    0x1e7ef, 0x1e7ff, 0x1e7ff, 0x1e8c5, 0x1e8c6, 0x1e8d7, 0x1e8ff, 0x1e94c, 0x1e94f, 0x1e95a,
    0x1e95d, 0x1e960, 0x1ec70, 0x1ecb5, 0x1ed00, 0x1ed3e, 0x1edff, 0x1ee04, 0x1ee04, 0x1ee20,
    0x1ee20, 0x1ee23, 0x1ee23, 0x1ee25, 0x1ee26, 0x1ee28, 0x1ee28, 0x1ee33, 0x1ee33, 0x1ee38,
    0x1ee38, 0x1ee3a, 0x1ee3a, 0x1ee3c, 0x1ee41, 0x1ee43, 0x1ee46, 0x1ee48, 0x1ee48, 0x1ee4a,
    0x1ee4a, 0x1ee4c, 0x1ee4c, 0x1ee50, 0x1ee50, 0x1ee53, 0x1ee53, 0x1ee55, 0x1ee56, 0x1ee58,
    0x1ee58, 0x1ee5a, 0x1ee5a, 0x1ee5c, 0x1ee5c, 0x1ee5e, 0x1ee5e, 0x1ee60, 0x1ee60, 0x1ee63,
    0x1ee63, 0x1ee65, 0x1ee66, 0x1ee6b, 0x1ee6b, 0x1ee73, 0x1ee73, 0x1ee78, 0x1ee78, 0x1ee7d,
    0x1ee7d, 0x1ee7f, 0x1ee7f, 0x1ee8a, 0x1ee8a, 0x1ee9c, 0x1eea0, 0x1eea4, 0x1eea4, 0x1eeaa,
    0x1eeaa, 0x1eebc, 0x1eeef, 0x1eef2, 0x1efff, 0x1f02c, 0x1f02f, 0x1f094, 0x1f09f, 0x1f0af,
    0x1f0b0, 0x1f0c0, 0x1f0c0, 0x1f0d0, 0x1f0d0, 0x1f0f6, 0x1f0ff, 0x1f1ae, 0x1f1e5, 0x1f203,
    0x1f20f, 0x1f23c, 0x1f23f, 0x1f249, 0x1f24f, 0x1f252, 0x1f25f, 0x1f266, 0x1f2ff, 0x1f6d8,
    0x1f6db, 0x1f6ed, 0x1f6ef, 0x1f6fd, 0x1f6ff, 0x1f777, 0x1f77a, 0x1f7da, 0x1f7df, 0x1f7ec,
    0x1f7ef, 0x1f7f1, 0x1f7ff, 0x1f80c, 0x1f80f, 0x1f848, 0x1f84f, 0x1f85a, 0x1f85f, 0x1f888,
    0x1f88f, 0x1f8ae, 0x1f8af, 0x1f8b2, 0x1f8ff, 0x1fa54, 0x1fa5f, 0x1fa6e, 0x1fa6f, 0x1fa7d,
    0x1fa7f, 0x1fa89, 0x1fa8f, 0x1fabe, 0x1fabe, 0x1fac6, 0x1facd, 0x1fadc, 0x1fadf, 0x1fae9,
    0x1faef, 0x1faf9, 0x1faff, 0x1fb93, 0x1fb93, 0x1fbcb, 0x1fbef, 0x1fbfa, 0x1ffff, 0x2a6e0,
    0x2a6ff, 0x2b73a, 0x2b73f, 0x2b81e, 0x2b81f, 0x2cea2, 0x2ceaf, 0x2ebe1, 0x2ebef, 0x2ee5e,
    0x2f7ff, 0x2fa1e, 0x2ffff, 0x3134b, 0x3134f, 0x323b0, 0xe00ff, 0xe01f0, 0x10ffff,
];
