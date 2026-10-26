//! Translation of a parsed Python pattern into fancy-regex syntax with the
//! matching semantics of CPython's `_sre` for `str` patterns.
//!
//! Every construct is emitted explicitly (no global flags), so Python's
//! lexically scoped flags map one-to-one:
//! - `\d \w \s` use Python's definitions (`str.isdecimal`, `str.isalnum() or '_'`,
//!   `str.isspace`), or their ASCII versions under `(?a)`.
//! - `\b \B` are look-arounds over that word class; `\B` never matches the empty
//!   string (CPython < 3.14).
//! - `$` is "end, or before a final `\n`"; `\Z` is `\z`.
//! - `(?i)` uses the engine's simple case folding, widened for the
//!   `i/I/İ/ı` quartet that CPython's `lower()` + `_casefix` treat as equal;
//!   under `(?a)` only ASCII letters fold.

use std::fmt::Write as _;
use std::sync::OnceLock;

use super::ast::{
    At, Category, ClassItem, MAXREPEAT, Node, RepeatKind, Width, WidthRules, flags, width,
};
use super::parser::Parsed;
use super::unicode::{UNRESOLVED_NAME, age};

/// A class that matches nothing (`FAILURE`, lone surrogates).
const NEVER: &str = r"[^\x{0}-\x{10FFFF}]";
/// Any character, including `\n`.
const ANY_CHAR: &str = r"[\x{0}-\x{10FFFF}]";

/// The categories as (nested) classes: `CH_UNICODE` (`str.isdecimal`,
/// `str.isspace`, `str.isalnum() or '_'`) and the ASCII variants. Unicode
/// properties are pinned to the running Python's Unicode version with `Age`
/// (cumulative), since regex-syntax may ship a newer one.
struct UnicodeClasses {
    digit: String,
    not_digit: String,
    word: String,
    not_word: String,
}

/// The [`UnicodeClasses`] of a Unicode version (see `unicode::unicode_version`).
fn unicode_classes(version: usize) -> &'static UnicodeClasses {
    static CLASSES: [OnceLock<UnicodeClasses>; 5] = [const { OnceLock::new() }; 5];
    CLASSES[version.min(CLASSES.len() - 1)].get_or_init(|| {
        let age = age(version);
        UnicodeClasses {
            digit: format!(r"[\p{{Nd}}&&\p{{Age={age}}}]"),
            not_digit: format!(r"[^[\p{{Nd}}&&\p{{Age={age}}}]]"),
            word: format!(r"[[\p{{L}}\p{{N}}\x{{5f}}]&&\p{{Age={age}}}]"),
            not_word: format!(r"[^[[\p{{L}}\p{{N}}\x{{5f}}]&&\p{{Age={age}}}]]"),
        }
    })
}
const UNI_SPACE: &str = r"[\x{9}-\x{d}\x{1c}-\x{20}\x{85}\x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}]";
const UNI_NOT_SPACE: &str = r"[^\x{9}-\x{d}\x{1c}-\x{20}\x{85}\x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}]";
const ASCII_DIGIT: &str = "[0-9]";
const ASCII_NOT_DIGIT: &str = "[^0-9]";
const ASCII_WORD: &str = r"[0-9A-Za-z\x{5f}]";
const ASCII_NOT_WORD: &str = r"[^0-9A-Za-z\x{5f}]";
const ASCII_SPACE: &str = r"[\x{9}-\x{d}\x{20}]";
const ASCII_NOT_SPACE: &str = r"[^\x{9}-\x{d}\x{20}]";

/// Characters CPython's case-insensitive matching treats as one class: `I` and
/// `İ` lower to `i`, and `_casefix` pairs `i` with `ı`. Simple case folding
/// (used by the engine) keeps only `i`/`I` together.
const I_GROUP: [u32; 4] = [0x49, 0x69, 0x130, 0x131];

/// Counted repeats above this bound are run in fancy-regex's VM when
/// translating with `harden`.
const HARDEN_MIN_COUNT: u32 = 16;
/// Repeats whose estimated unrolled NFA weight exceeds this always run in the
/// VM: unrolling e.g. `\w{1,64}` (Unicode classes) costs tens of milliseconds
/// to compile, and `\w{1,255}` exceeds the delegate's size limit.
const HARDEN_WEIGHT: u64 = 4000;

/// Rough relative NFA size of matching `nodes` once, for [`HARDEN_WEIGHT`].
fn weight(nodes: &[Node], f: u32) -> u64 {
    let ascii = f & flags::ASCII != 0;
    nodes.iter().fold(0u64, |acc, node| {
        let w = match node {
            Node::Literal(_) => 1,
            Node::Any => 20,
            Node::Class { negate, items } => {
                let heavy_cat = !ascii
                    && items.iter().any(|i| {
                        matches!(
                            i,
                            ClassItem::Category(
                                Category::Word
                                    | Category::NotWord
                                    | Category::NotDigit
                                    | Category::NotSpace
                            )
                        )
                    });
                let non_ascii = items.iter().any(|i| match *i {
                    ClassItem::Literal(c) | ClassItem::Range(_, c) => c > 0x7f,
                    ClassItem::Category(_) => !ascii,
                });
                if heavy_cat {
                    400
                } else if *negate || non_ascii {
                    20
                } else {
                    1
                }
            }
            Node::Repeat { min, max, body, .. } => {
                let bound = if *max == MAXREPEAT { *min + 1 } else { *max };
                weight(body, f).saturating_mul(u64::from(bound))
            }
            Node::Group { add, del, body, .. } => weight(body, flags::combine(f, *add, *del)),
            Node::Atomic { body, .. } | Node::Look { body, .. } => weight(body, f),
            Node::Branch { alts, .. } => alts.iter().map(|a| weight(a, f)).sum(),
            Node::GroupRefExists { yes, no, .. } => {
                weight(yes, f) + no.as_deref().map_or(0, |n| weight(n, f))
            }
            Node::At(_) | Node::GroupRef(_) => 0,
        };
        acc.saturating_add(w)
    })
}

/// Translates `parsed`. With `harden`, all large counted repeats are forced
/// onto fancy-regex's backtracking VM (used when the default translation still
/// exceeds the delegate engine's size limit).
pub(crate) fn translate(parsed: &Parsed, harden: bool) -> Result<String, String> {
    if parsed.too_deep {
        return Err("pattern nests too deeply for fancy-regex (64 levels)".into());
    }
    let mut tr = Translator {
        out: String::new(),
        open_groups: Vec::new(),
        harden,
        empty_non_boundary: parsed.empty_non_boundary,
        unicode: unicode_classes(parsed.unicode_version),
    };
    if let Some((negate, items)) = charset_prefix_quirk(parsed) {
        // `_compile_info` compiles the leading charset with the global flags,
        // and `sre_search` only tries start positions matching it.
        tr.out.push_str("(?=");
        tr.class(negate, &items, parsed.flags & !flags::IGNORECASE);
        tr.out.push(')');
    }
    tr.seq(&parsed.nodes, parsed.flags, true)?;
    Ok(tr.out)
}

/// The nodes of `seq` with plain `(?:...)` groups spliced in, as `_parse` does.
fn flatten(seq: &[Node]) -> Vec<&Node> {
    let mut out = Vec::with_capacity(seq.len());
    for node in seq {
        match node {
            Node::Group {
                index: None,
                add: 0,
                del: 0,
                body,
                ..
            } => out.extend(flatten(body)),
            _ => out.push(node),
        }
    }
    out
}

/// The `(op, av)` value `_parse` would build for a node, for the equality
/// tests in `_parse_sub`; `None` for nodes holding a `SubPattern`, which only
/// compare equal to themselves.
#[derive(PartialEq)]
enum PyCode {
    Literal(u32),
    NotLiteral(u32),
    In(bool, Vec<ClassItem>),
    Other(String),
}

fn py_code(node: &Node) -> Option<PyCode> {
    Some(match node {
        Node::Literal(c) => PyCode::Literal(*c),
        Node::Class { negate, items } => {
            let mut uniq: Vec<ClassItem> = Vec::with_capacity(items.len());
            for item in items {
                if !uniq.contains(item) {
                    uniq.push(*item);
                }
            }
            match (negate, uniq.as_slice()) {
                (false, [ClassItem::Literal(c)]) => PyCode::Literal(*c),
                (true, [ClassItem::Literal(c)]) => PyCode::NotLiteral(*c),
                _ => PyCode::In(*negate, uniq),
            }
        }
        Node::Any | Node::At(_) | Node::GroupRef(_) => PyCode::Other(format!("{node:?}")),
        _ => return None,
    })
}

/// The leading `IN` operand `_compiler._get_charset_prefix` would find (after
/// the alternation rewrites of `_parse_sub`), with the flags in effect there.
fn leading_charset(seq: &[Node], f: u32) -> Option<(bool, Vec<ClassItem>, u32)> {
    let flat = flatten(seq);
    match *flat.first()? {
        Node::Group { add, del, body, .. } => leading_charset(body, flags::combine(f, *add, *del)),
        node @ Node::Class { .. } => match py_code(node)? {
            PyCode::In(negate, items) => Some((negate, items, f)),
            _ => None,
        },
        Node::Branch { alts, .. } => {
            let alts: Vec<Vec<&Node>> = alts.iter().map(|a| flatten(a)).collect();
            let firsts: Vec<Option<PyCode>> = alts
                .iter()
                .map(|a| a.first().and_then(|n| py_code(n)))
                .collect();
            if firsts.iter().all(Option::is_some) && firsts.windows(2).all(|w| w[0] == w[1]) {
                // common prefix moved out of the branch
                return match firsts.into_iter().next().flatten()? {
                    PyCode::In(negate, items) => Some((negate, items, f)),
                    _ => None,
                };
            }
            // a branch of single literals / non-negated sets becomes one set
            let mut items = Vec::new();
            for alt in &alts {
                match (alt.as_slice(), alt.first().and_then(|n| py_code(n))) {
                    ([_], Some(PyCode::Literal(c))) => items.push(ClassItem::Literal(c)),
                    ([_], Some(PyCode::In(false, its))) => items.extend(its),
                    _ => return None,
                }
            }
            let mut uniq: Vec<ClassItem> = Vec::with_capacity(items.len());
            for item in items {
                if !uniq.contains(&item) {
                    uniq.push(item);
                }
            }
            Some((false, uniq, f))
        }
        _ => None,
    }
}

/// `_sre.unicode_iscased` / `ascii_iscased` (simple case mappings).
fn iscased(cp: u32, ascii: bool) -> bool {
    let Some(c) = char::from_u32(cp) else {
        return false;
    };
    if ascii {
        return c.is_ascii_alphabetic();
    }
    // Simple mappings: a multi-character full mapping means "none", except
    // U+0130 whose simple lowercase is 'i'.
    fn single(mut it: impl Iterator<Item = char>) -> Option<char> {
        let first = it.next()?;
        it.next().is_none().then_some(first)
    }
    let lower = if c == '\u{130}' {
        'i'
    } else {
        single(c.to_lowercase()).unwrap_or(c)
    };
    let upper = single(c.to_uppercase()).unwrap_or(c);
    lower != c || upper != c
}

/// CPython quirk: when a pattern starts with a set containing `\d \s \w` (or
/// their negations) inside a scoped `(?a:...)`/`(?u:...)` group that flips
/// ASCII/Unicode, the search prefilter built by `_compile_info` uses the
/// *global* meaning of those categories, e.g. `re.search(r'(?a)(?u:\w)', 'é')`
/// is `None`. Returns that prefilter set when it differs from the pattern's.
fn charset_prefix_quirk(parsed: &Parsed) -> Option<(bool, Vec<ClassItem>)> {
    if width(&parsed.nodes, &parsed.groupwidths, WidthRules::Current).0 == 0 {
        return None; // no info-block prefix for patterns that can match empty
    }
    let (negate, items, f) = leading_charset(&parsed.nodes, parsed.flags)?;
    let has_category = items.iter().any(|i| matches!(i, ClassItem::Category(_)));
    if !has_category || (f ^ parsed.flags) & flags::ASCII == 0 {
        return None;
    }
    if f & flags::IGNORECASE != 0 {
        let ascii = f & flags::ASCII != 0;
        let cased = items.iter().any(|i| match *i {
            ClassItem::Literal(c) => iscased(c, ascii),
            ClassItem::Range(lo, hi) => hi > 0xffff || (lo..=hi).any(|c| iscased(c, ascii)),
            ClassItem::Category(_) => false,
        });
        if cased {
            return None;
        }
    }
    Some((negate, items))
}

struct Translator {
    out: String,
    /// Capturing groups enclosing the node being translated.
    open_groups: Vec<u32>,
    harden: bool,
    /// `\B` matches the empty string (Python 3.14).
    empty_non_boundary: bool,
    /// The categories of the running Python's Unicode version.
    unicode: &'static UnicodeClasses,
}

fn has_capture(nodes: &[Node]) -> bool {
    nodes.iter().any(|n| match n {
        Node::Group { index: Some(_), .. } => true,
        Node::Group { body, .. }
        | Node::Atomic { body, .. }
        | Node::Repeat { body, .. }
        | Node::Look { body, .. } => has_capture(body),
        Node::Branch { alts, .. } => alts.iter().any(|a| has_capture(a)),
        Node::GroupRefExists { yes, no, .. } => {
            has_capture(yes) || no.as_deref().is_some_and(has_capture)
        }
        _ => false,
    })
}

fn is_surrogate(cp: u32) -> bool {
    (0xD800..=0xDFFF).contains(&cp)
}

fn push_char(out: &mut String, cp: u32) {
    match char::from_u32(cp) {
        Some(c) if c.is_ascii_alphanumeric() => out.push(c),
        _ => {
            let _ = write!(out, "\\x{{{cp:x}}}");
        }
    }
}

fn category_class(cat: Category, ascii: bool, unicode: &'static UnicodeClasses) -> &'static str {
    match (cat, ascii) {
        (Category::Digit, false) => &unicode.digit,
        (Category::NotDigit, false) => &unicode.not_digit,
        (Category::Space, false) => UNI_SPACE,
        (Category::NotSpace, false) => UNI_NOT_SPACE,
        (Category::Word, false) => &unicode.word,
        (Category::NotWord, false) => &unicode.not_word,
        (Category::Digit, true) => ASCII_DIGIT,
        (Category::NotDigit, true) => ASCII_NOT_DIGIT,
        (Category::Space, true) => ASCII_SPACE,
        (Category::NotSpace, true) => ASCII_NOT_SPACE,
        (Category::Word, true) => ASCII_WORD,
        (Category::NotWord, true) => ASCII_NOT_WORD,
    }
}

/// Appends the categories as nested-class members.
fn push_categories(
    out: &mut String,
    cats: &[Category],
    ascii: bool,
    unicode: &'static UnicodeClasses,
) {
    for &cat in cats {
        out.push_str(category_class(cat, ascii, unicode));
    }
}

/// Appends literal ranges as class members, dropping lone surrogates (which
/// cannot occur in a Rust `&str`).
fn push_ranges(out: &mut String, ranges: &[(u32, u32)]) {
    for &(lo, hi) in ranges {
        for (a, b) in [(lo, hi.min(0xD7FF)), (lo.max(0xE000), hi)] {
            if a > b {
                continue;
            }
            push_char(out, a);
            if b > a {
                out.push('-');
                push_char(out, b);
            }
        }
    }
}

fn ranges_have_chars(ranges: &[(u32, u32)]) -> bool {
    ranges
        .iter()
        .any(|&(lo, hi)| lo <= hi.min(0xD7FF) || lo.max(0xE000) <= hi)
}

impl Translator {
    fn seq(&mut self, nodes: &[Node], f: u32, tail: bool) -> Result<(), String> {
        let last = nodes.len().saturating_sub(1);
        for (i, node) in nodes.iter().enumerate() {
            self.node(node, f, tail && i == last)?;
        }
        Ok(())
    }

    /// Emits `nodes` as a single atom suitable for a quantifier.
    fn atom(&mut self, nodes: &[Node], f: u32) -> Result<(), String> {
        let single = matches!(
            nodes,
            [Node::Literal(_)
                | Node::Any
                | Node::Class { .. }
                | Node::Group { .. }
                | Node::Atomic { .. }
                | Node::GroupRef(_)]
        );
        if single {
            self.seq(nodes, f, false)
        } else {
            self.out.push_str("(?:");
            self.seq(nodes, f, false)?;
            self.out.push(')');
            Ok(())
        }
    }

    fn node(&mut self, node: &Node, f: u32, tail: bool) -> Result<(), String> {
        match node {
            Node::Literal(cp) => self.literal(*cp, f)?,
            Node::Any => self.out.push_str(if f & flags::DOTALL != 0 {
                "(?s:.)"
            } else {
                "."
            }),
            Node::Class { negate, items } => self.class(*negate, items, f),
            Node::At(at) => self.at(*at, f, tail),
            Node::Repeat {
                kind,
                min,
                max,
                body,
                body_w,
            } => self.repeat(*kind, (*min, *max), body, *body_w, f)?,
            Node::Group {
                index,
                add,
                del,
                body,
                ..
            } => {
                self.out.push_str(if index.is_some() { "(" } else { "(?:" });
                if let Some(g) = index {
                    self.open_groups.push(*g);
                }
                self.seq(body, flags::combine(f, *add, *del), tail)?;
                if index.is_some() {
                    self.open_groups.pop();
                }
                self.out.push(')');
            }
            Node::Atomic { body, .. } => {
                self.out.push_str("(?>");
                self.seq(body, f, tail)?;
                self.out.push(')');
            }
            Node::Look {
                behind,
                negate,
                body,
                ..
            } => {
                if *negate && body.is_empty() {
                    // `(?!)` compiles to FAILURE
                    self.out.push_str(NEVER);
                    return Ok(());
                }
                self.out.push_str(match (behind, negate) {
                    (false, false) => "(?=",
                    (false, true) => "(?!",
                    (true, false) => "(?<=",
                    (true, true) => "(?<!",
                });
                // The end of a look-ahead body is a tail position; a look-behind's is not.
                self.seq(body, f, !behind)?;
                self.out.push(')');
            }
            Node::Branch { alts, .. } => {
                self.out.push_str("(?:");
                for (i, alt) in alts.iter().enumerate() {
                    if i > 0 {
                        self.out.push('|');
                    }
                    self.seq(alt, f, tail)?;
                }
                self.out.push(')');
            }
            Node::GroupRef(g) => {
                let open = if f & flags::IGNORECASE != 0 {
                    "(?i:"
                } else {
                    "(?:"
                };
                let _ = write!(self.out, "{open}\\{g})");
            }
            Node::GroupRefExists { group, yes, no, .. } => {
                if self.open_groups.contains(group) {
                    // `_sre` treats a group as unset until it closes (start and
                    // end marks, end >= start); fancy-regex only checks the start.
                    return Err(format!(
                        "conditional (?({group})...) inside group {group} is not supported"
                    ));
                }
                // fancy-regex's `(?(g)yes|no)` leaves an unbalanced atomic
                // marker on its "no" path (breaking enclosing `(?>...)`), and
                // reduces `(?(g)|)` to "group g matched". Spell the
                // conditional with the bare existence test instead.
                let mark = self.out.len();
                let _ = write!(self.out, "(?:(?({group}))");
                let yes_start = self.out.len();
                self.seq(yes, f, tail)?;
                let yes_empty = self.out.len() == yes_start;
                let _ = write!(self.out, "|(?!(?({group})))");
                let no_start = self.out.len();
                if let Some(no) = no {
                    self.seq(no, f, tail)?;
                }
                if yes_empty && self.out.len() == no_start {
                    self.out.truncate(mark); // matches the empty string either way
                } else {
                    self.out.push(')');
                }
            }
        }
        Ok(())
    }

    fn repeat(
        &mut self,
        kind: RepeatKind,
        (min, max): (u32, u32),
        body: &[Node],
        body_w: Width,
        f: u32,
    ) -> Result<(), String> {
        if max == 0 {
            // `x{0}` matches the empty string without setting groups. Spelling
            // it out would make fancy-regex see a variable-width look-behind;
            // groups inside must still be declared to keep the numbering.
            if has_capture(body) {
                self.out.push_str("(?!");
                self.out.push_str(NEVER);
                self.atom(body, f)?;
                self.out.push(')');
            }
            return Ok(());
        }
        if body_w.1 == 0 {
            // `_sre` stops iterating once an iteration makes no progress, so a
            // zero-width body runs at most once beyond `min`, and repeating it
            // at one position cannot change the outcome. fancy-regex rejects
            // quantified look-arounds and empty groups, so spell that out.
            if min > 0 {
                return self.seq(body, f, false);
            }
            if !has_capture(body) {
                return Ok(()); // optional and side-effect free
            }
            // Optional, keeping the captures an attempt may set.
            let (open, lazy) = match kind {
                RepeatKind::Greedy => ("(?:", false),
                RepeatKind::Lazy => ("(?:", true),
                RepeatKind::Possessive => ("(?>", false),
            };
            self.out.push_str(open);
            if lazy {
                self.out.push('|');
            }
            self.seq(body, f, false)?;
            if !lazy {
                self.out.push('|');
            }
            self.out.push(')');
            return Ok(());
        }
        let possessive = kind == RepeatKind::Possessive;
        if possessive {
            self.out.push_str("(?>");
        }
        let bound = if max == MAXREPEAT { min } else { max };
        let heavy = weight(body, f).saturating_mul(u64::from(bound)) > HARDEN_WEIGHT;
        if heavy || (self.harden && bound > HARDEN_MIN_COUNT) {
            // An empty look-ahead makes the body "hard", so fancy-regex runs the
            // repeat with counters in its VM instead of unrolling it into an
            // NFA that can exceed the delegate size limit (e.g. `\w{1,1000}`).
            self.out.push_str("(?:(?=)");
            self.seq(body, f, false)?;
            self.out.push(')');
        } else {
            self.atom(body, f)?;
        }
        let out = &mut self.out;
        match (min, max) {
            (0, MAXREPEAT) => out.push('*'),
            (1, MAXREPEAT) => out.push('+'),
            (0, 1) => out.push('?'),
            (m, MAXREPEAT) => {
                let _ = write!(out, "{{{m},}}");
            }
            (m, n) if m == n => {
                let _ = write!(out, "{{{m}}}");
            }
            (m, n) => {
                let _ = write!(out, "{{{m},{n}}}");
            }
        }
        if kind == RepeatKind::Lazy {
            out.push('?');
        }
        if possessive {
            out.push(')');
        }
        Ok(())
    }

    fn literal(&mut self, cp: u32, f: u32) -> Result<(), String> {
        if cp == UNRESOLVED_NAME {
            return Err("\\N{...} named escapes are not resolved (no Unicode name table)".into());
        }
        if is_surrogate(cp) {
            self.out.push_str(NEVER);
            return Ok(());
        }
        if f & flags::IGNORECASE != 0 {
            if f & flags::ASCII != 0 {
                if let Some(c) = char::from_u32(cp).filter(char::is_ascii_alphabetic) {
                    let _ = write!(
                        self.out,
                        "[{}{}]",
                        c.to_ascii_lowercase(),
                        c.to_ascii_uppercase()
                    );
                    return Ok(());
                }
            } else if I_GROUP.contains(&cp) {
                self.out.push_str(r"[iI\x{130}\x{131}]");
                return Ok(());
            } else {
                self.out.push_str("(?i:");
                push_char(&mut self.out, cp);
                self.out.push(')');
                return Ok(());
            }
        }
        push_char(&mut self.out, cp);
        Ok(())
    }

    fn class(&mut self, negate: bool, items: &[ClassItem], f: u32) {
        let ascii = f & flags::ASCII != 0;
        let ignore = f & flags::IGNORECASE != 0;
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        let mut cats: Vec<Category> = Vec::new();
        for item in items {
            match *item {
                ClassItem::Literal(c) => ranges.push((c, c)),
                ClassItem::Range(a, b) => ranges.push((a, b)),
                ClassItem::Category(c) => cats.push(c),
            }
        }
        // Case-insensitive matching handled by the engine applies to literal
        // members only; categories are never case-folded by `_sre` here.
        let mut fold = false;
        if ignore {
            if ascii {
                let mut extra = Vec::new();
                for &(lo, hi) in &ranges {
                    for (start, end, delta) in [(0x41u32, 0x5Au32, 0x20i64), (0x61, 0x7A, -0x20)] {
                        let (a, b) = (lo.max(start), hi.min(end));
                        if a <= b {
                            let shift = |x: u32| (i64::from(x) + delta) as u32;
                            extra.push((shift(a), shift(b)));
                        }
                    }
                }
                ranges.extend(extra);
            } else {
                fold = !ranges.is_empty();
                let covers_i = ranges
                    .iter()
                    .any(|&(lo, hi)| I_GROUP.iter().any(|&c| lo <= c && c <= hi));
                if covers_i {
                    ranges.extend(I_GROUP.iter().map(|&c| (c, c)));
                }
            }
        }
        let has_lits = ranges_have_chars(&ranges);
        let unicode = self.unicode;
        let out = &mut self.out;
        if !has_lits && cats.is_empty() {
            out.push_str(if negate { ANY_CHAR } else { NEVER });
            return;
        }
        if fold && has_lits && !cats.is_empty() {
            // Fold the literal part only.
            if negate {
                out.push_str("(?:(?![");
                push_categories(out, &cats, ascii, unicode);
                out.push_str("])(?i:[^");
                push_ranges(out, &ranges);
                out.push_str("]))");
            } else {
                out.push_str("(?:(?i:[");
                push_ranges(out, &ranges);
                out.push_str("])|[");
                push_categories(out, &cats, ascii, unicode);
                out.push_str("])");
            }
            return;
        }
        let wrap = fold && has_lits;
        if wrap {
            out.push_str("(?i:");
        }
        out.push('[');
        if negate {
            out.push('^');
        }
        push_ranges(out, &ranges);
        push_categories(out, &cats, ascii, unicode);
        out.push(']');
        if wrap {
            out.push(')');
        }
    }

    fn at(&mut self, at: At, f: u32, tail: bool) {
        let multiline = f & flags::MULTILINE != 0;
        let word = if f & flags::ASCII != 0 {
            ASCII_WORD
        } else {
            &self.unicode.word
        };
        let out = &mut self.out;
        match at {
            At::Beginning if multiline => out.push_str("(?m:^)"),
            At::Beginning | At::BeginningString => out.push_str(r"\A"),
            At::End if multiline => out.push_str("(?m:$)"),
            // Consuming the optional newline is equivalent when nothing follows.
            At::End if tail => out.push_str(r"\n?\z"),
            At::End => out.push_str(r"(?=\n?\z)"),
            At::EndString => out.push_str(r"\z"),
            At::Boundary => {
                let _ = write!(out, r"(?:(?<={word})(?!{word})|(?<!{word})(?={word}))");
            }
            // Until Python 3.14, `\B` never matches the empty string.
            At::NonBoundary if self.empty_non_boundary => {
                let _ = write!(out, r"(?:(?<={word})(?={word})|(?<!{word})(?!{word}))");
            }
            At::NonBoundary => {
                let _ = write!(
                    out,
                    r"(?:(?<={word})(?={word})|(?<!{word})(?!{word})(?!\A\z))"
                );
            }
        }
    }
}
