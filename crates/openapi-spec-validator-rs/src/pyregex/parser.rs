//! Port of CPython 3.13 `re/_parser.py` (plus the compile-time checks of
//! `re/_compiler.py`). Control flow, token look-ahead and error offsets follow
//! the Python source statement by statement, because the *first* error raised
//! (and its position) depends on that order. Where the syntax differs in
//! Python 3.10 to 3.12 or 3.14, the running version's rules apply
//! (`context::python_minor`).
//!
//! The mutual recursion of `_parse_sub` and `_parse` is run on an explicit
//! stack ([`Frame`]) so that deeply nested patterns cannot overflow the Rust
//! stack; Python's own recursion limit is modelled by [`MAX_PY_FRAMES`].

use super::ast::{
    At, Category, ClassItem, MAXCODE, MAXGROUPS, MAXREPEAT, Node, RepeatKind, Width, WidthRules,
    branch_width, dismantle, flags, width,
};
use super::unicode::{is_identifier, lookup_name, py_isalpha, py_repr, unicode_version};
use super::{ErrorKind, PyRegexError};
use crate::formats::decimal_value;

type PResult<T> = Result<T, PyRegexError>;

/// `SPECIAL_CHARS` minus the backslash (which always forms an escape token).
const SPECIAL_CHARS: &str = ".[{()*+?^$|";
/// `WHITESPACE`
const WHITESPACE: &str = " \t\n\r\x0b\x0c";
/// Python frames of recursive descent (`_parse_sub` + `_parse` per group, one
/// `_parse` per conditional branch) available before `RecursionError`.
/// Calibrated on openapi-spec-validator 0.9.0, whose `re.compile` call sits at
/// a fixed stack depth: 485 nested groups compile, 486 raise. (A bare
/// `re.compile` at module level allows ~10 more levels.)
const MAX_PY_FRAMES: usize = 972;

/// A tokenizer token: a single character, or a backslash plus one character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    Ch(char),
    Esc(char),
}

impl Tok {
    fn len(self) -> usize {
        match self {
            Tok::Ch(_) => 1,
            Tok::Esc(_) => 2,
        }
    }

    fn push_to(self, s: &mut String) {
        match self {
            Tok::Ch(c) => s.push(c),
            Tok::Esc(c) => {
                s.push('\\');
                s.push(c);
            }
        }
    }

    fn text(self) -> String {
        let mut s = String::new();
        self.push_to(&mut s);
        s
    }

    fn is(self, c: char) -> bool {
        self == Tok::Ch(c)
    }

    fn ch_in(self, set: &str) -> bool {
        matches!(self, Tok::Ch(c) if set.contains(c))
    }

    fn digit(self) -> Option<char> {
        match self {
            Tok::Ch(c) if c.is_ascii_digit() => Some(c),
            _ => None,
        }
    }
}

fn re_error(msg: impl Into<String>, pos: usize) -> PyRegexError {
    PyRegexError {
        kind: ErrorKind::Re,
        msg: msg.into(),
        pos: Some(pos),
    }
}

fn other_error(kind: ErrorKind, msg: impl Into<String>) -> PyRegexError {
    PyRegexError {
        kind,
        msg: msg.into(),
        pos: None,
    }
}

/// `int(digits)` for an ASCII digit string, saturating at `u64::MAX`; raises
/// CPython's `ValueError` for strings beyond the int/str conversion limit.
fn py_int(digits: &str) -> PResult<u64> {
    if digits.len() > crate::pyobj::MAX_STR_DIGITS {
        return Err(other_error(
            ErrorKind::Value,
            crate::pyobj::int_from_str_limit(digits.len()),
        ));
    }
    Ok(digits.bytes().fold(0u64, |acc, b| {
        acc.saturating_mul(10).saturating_add(u64::from(b - b'0'))
    }))
}

/// `str(int(text))` for any `str`, as Python 3.11 and earlier read a numeric group
/// condition: surrounding whitespace, a sign, underscores between digits and non-ASCII
/// decimal digits are accepted. `None` where `int()` raises `ValueError`, including
/// beyond the int/str conversion limit.
fn py_int_text(text: &str) -> Option<String> {
    let ascii: String = text
        .chars()
        .map(|c| {
            if c.is_whitespace() {
                ' '
            } else if let Some(d) = decimal_value(c) {
                char::from_digit(d, 10).unwrap_or('?')
            } else if c.is_ascii() {
                c
            } else {
                '?'
            }
        })
        .collect();
    let trimmed = ascii.trim_matches(' ');
    let (negative, body) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, &trimmed[1..]),
        Some(b'+') => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    let well_formed = body.starts_with(|c: char| c.is_ascii_digit())
        && body.ends_with(|c: char| c.is_ascii_digit())
        && !body.contains("__")
        && body.bytes().all(|b| b.is_ascii_digit() || b == b'_');
    let digits: String = body.chars().filter(|&c| c != '_').collect();
    if !well_formed || digits.len() > crate::pyobj::MAX_STR_DIGITS {
        return None;
    }
    let value = int_text(&digits);
    Some(if negative && value != "0" {
        format!("-{value}")
    } else {
        value.to_owned()
    })
}

/// `str(int(digits))` (leading zeros stripped).
fn int_text(digits: &str) -> &str {
    let t = digits.trim_start_matches('0');
    if t.is_empty() { "0" } else { t }
}

fn hex_value(digits: &str) -> u32 {
    digits
        .chars()
        .fold(0, |acc, c| acc * 16 + c.to_digit(16).unwrap_or(0))
}

fn oct_value(digits: &str) -> u32 {
    digits
        .chars()
        .fold(0, |acc, c| acc * 8 + c.to_digit(8).unwrap_or(0))
}

fn is_oct(c: char) -> bool {
    ('0'..='7').contains(&c)
}

/// `_parser.Tokenizer`
struct Tokenizer {
    chars: Vec<char>,
    index: usize,
    next: Option<Tok>,
}

impl Tokenizer {
    fn new(pattern: &str) -> PResult<Self> {
        let mut t = Tokenizer {
            chars: pattern.chars().collect(),
            index: 0,
            next: None,
        };
        t.advance()?;
        Ok(t)
    }

    /// `Tokenizer.__next`
    fn advance(&mut self) -> PResult<()> {
        let Some(&c) = self.chars.get(self.index) else {
            self.next = None;
            return Ok(());
        };
        if c == '\\' {
            let Some(&e) = self.chars.get(self.index + 1) else {
                return Err(re_error(
                    "bad escape (end of pattern)",
                    self.chars.len() - 1,
                ));
            };
            self.index += 2;
            self.next = Some(Tok::Esc(e));
        } else {
            self.index += 1;
            self.next = Some(Tok::Ch(c));
        }
        Ok(())
    }

    fn match_(&mut self, c: char) -> PResult<bool> {
        if self.next == Some(Tok::Ch(c)) {
            self.advance()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn get(&mut self) -> PResult<Option<Tok>> {
        let this = self.next;
        self.advance()?;
        Ok(this)
    }

    fn getwhile(&mut self, n: usize, pred: impl Fn(char) -> bool) -> PResult<String> {
        let mut result = String::new();
        for _ in 0..n {
            match self.next {
                Some(Tok::Ch(c)) if pred(c) => {
                    result.push(c);
                    self.advance()?;
                }
                _ => break,
            }
        }
        Ok(result)
    }

    /// Returns the name and its length in characters.
    fn getuntil(&mut self, terminator: char, name: &str) -> PResult<(String, usize)> {
        let mut result = String::new();
        let mut len = 0;
        loop {
            let c = self.next;
            self.advance()?;
            match c {
                None => {
                    if len == 0 {
                        return Err(self.error(format!("missing {name}"), 0));
                    }
                    return Err(self.error(format!("missing {terminator}, unterminated name"), len));
                }
                Some(t) if t.is(terminator) => {
                    if len == 0 {
                        return Err(self.error(format!("missing {name}"), 1));
                    }
                    break;
                }
                Some(t) => {
                    t.push_to(&mut result);
                    len += t.len();
                }
            }
        }
        Ok((result, len))
    }

    fn tell(&self) -> usize {
        self.index - self.next.map_or(0, Tok::len)
    }

    fn seek(&mut self, index: usize) -> PResult<()> {
        self.index = index;
        self.advance()
    }

    fn error(&self, msg: impl Into<String>, offset: usize) -> PyRegexError {
        re_error(msg, self.tell().saturating_sub(offset))
    }

    fn checkgroupname(&self, name: &str, len: usize, offset: usize) -> PResult<()> {
        if !is_identifier(name) {
            return Err(self.error(
                format!("bad character in group name {}", py_repr(name)),
                len + offset,
            ));
        }
        Ok(())
    }
}

/// `_parser.State`
struct State {
    flags: u32,
    groupdict: Vec<(String, u32)>,
    /// `groupwidths`; index 0 is group 0, `None` while a group is open.
    groupwidths: Vec<Option<Width>>,
    lookbehindgroups: Option<usize>,
    /// `grouprefpos`, in insertion order.
    grouprefpos: Vec<(u64, usize)>,
}

impl State {
    fn groups(&self) -> usize {
        self.groupwidths.len()
    }

    fn group_index(&self, name: &str) -> Option<u32> {
        self.groupdict
            .iter()
            .find_map(|(n, g)| (n == name).then_some(*g))
    }

    fn checkgroup(&self, gid: u64) -> bool {
        usize::try_from(gid)
            .ok()
            .and_then(|g| self.groupwidths.get(g))
            .is_some_and(Option::is_some)
    }

    fn checklookbehindgroup(&self, gid: u64, source: &Tokenizer) -> PResult<()> {
        if let Some(lb) = self.lookbehindgroups {
            if !self.checkgroup(gid) {
                return Err(source.error("cannot refer to an open group", 0));
            }
            if gid >= lb as u64 {
                return Err(source.error(
                    "cannot refer to group defined in the same lookbehind subpattern",
                    0,
                ));
            }
        }
        Ok(())
    }
}

/// Result of a successful `re.compile`.
pub(crate) struct Parsed {
    /// The pattern tree; emptied (see `too_deep`) for very deep patterns.
    pub(crate) nodes: Vec<Node>,
    /// Global flags after `fix_flags` (always UNICODE or ASCII for str patterns).
    pub(crate) flags: u32,
    /// `State.groupwidths` (index 0 is the whole pattern, unused).
    pub(crate) groupwidths: Vec<Option<Width>>,
    /// Nesting exceeded what fancy-regex accepts (64 levels); the tree was
    /// discarded and the pattern cannot be matched.
    pub(crate) too_deep: bool,
    /// `\B` matches the empty string, as it does from Python 3.14.
    pub(crate) empty_non_boundary: bool,
    /// The Unicode version of the Python whose rules applied (`unicode::unicode_version`).
    pub(crate) unicode_version: usize,
}

impl Drop for Parsed {
    fn drop(&mut self) {
        dismantle(std::mem::take(&mut self.nodes));
    }
}

/// Deepest nesting kept for translation; fancy-regex rejects more than 64.
const MAX_TRANSLATABLE_DEPTH: usize = 64;

/// `re.compile(pattern)`: `_parser.parse` followed by the checks `_compiler`
/// performs while emitting code.
pub(crate) fn compile(pattern: &str) -> PResult<Parsed> {
    let minor = crate::context::python_minor();
    let mut p = Parser::new(pattern, minor, false)?;
    let mut result = p.parse_all();
    if p.restart_verbose {
        // Python 3.10: `(?x)` after the start makes the whole pattern verbose, so
        // `parse` starts again from the beginning (its `Verbose` exception).
        p = Parser::new(pattern, minor, true)?;
        result = p.parse_all();
    }
    let nodes = result?;
    let mut parsed = Parsed {
        nodes,
        flags: 0,
        groupwidths: Vec::new(),
        too_deep: p.max_depth > MAX_TRANSLATABLE_DEPTH,
        empty_non_boundary: minor >= 14,
        unicode_version: unicode_version(minor),
    };
    parsed.flags = fix_flags(p.state.flags)?;
    if p.source.next.is_some() {
        return Err(p.source.error("unbalanced parenthesis", 0));
    }
    for &(g, pos) in &p.state.grouprefpos {
        if g >= p.state.groups() as u64 {
            return Err(re_error(format!("invalid group reference {g}"), pos));
        }
    }
    check_compiled(&parsed.nodes, parsed.flags, minor)?;
    if parsed.too_deep {
        dismantle(std::mem::take(&mut parsed.nodes));
    }
    parsed.groupwidths = p.state.groupwidths;
    Ok(parsed)
}

/// `_parser.fix_flags` for a str pattern.
fn fix_flags(f: u32) -> PResult<u32> {
    if f & flags::LOCALE != 0 {
        return Err(other_error(
            ErrorKind::Value,
            "cannot use LOCALE flag with a str pattern",
        ));
    }
    if f & flags::ASCII == 0 {
        Ok(f | flags::UNICODE)
    } else if f & flags::UNICODE != 0 {
        Err(other_error(
            ErrorKind::Value,
            "ASCII and UNICODE flags are incompatible",
        ))
    } else {
        Ok(f)
    }
}

/// The checks in `_compiler._compile`, in emission (pre-)order: look-behind widths, and
/// repeats under `(?t)` (Python 3.12 and earlier). Python 3.10's `_sre` then rejects a
/// look-behind of 2**31 characters or more, which 3.11 no longer emits that way.
fn check_compiled(nodes: &[Node], f: u32, minor: u32) -> PResult<()> {
    let mut sre_code_too_wide = false;
    let mut work: Vec<std::slice::Iter<'_, Node>> = vec![nodes.iter()];
    while let Some(iter) = work.last_mut() {
        let Some(node) = iter.next() else {
            work.pop();
            continue;
        };
        match node {
            Node::Look {
                behind,
                body,
                body_w,
                ..
            } => {
                if *behind {
                    let (lo, hi) = *body_w;
                    if lo > MAXCODE && minor >= 11 {
                        return Err(other_error(ErrorKind::Re, "looks too much behind"));
                    }
                    if lo != hi {
                        return Err(other_error(
                            ErrorKind::Re,
                            "look-behind requires fixed-width pattern",
                        ));
                    }
                    sre_code_too_wide |= minor <= 10 && lo >= 1 << 31;
                }
                work.push(body.iter());
            }
            Node::Repeat { kind, body, .. } => {
                if f & flags::TEMPLATE != 0 {
                    let op = match kind {
                        RepeatKind::Greedy => "MAX_REPEAT",
                        RepeatKind::Lazy => "MIN_REPEAT",
                        RepeatKind::Possessive => "POSSESSIVE_REPEAT",
                    };
                    return Err(other_error(
                        ErrorKind::Re,
                        format!("internal: unsupported template operator {op}"),
                    ));
                }
                work.push(body.iter());
            }
            Node::Group { body, .. } | Node::Atomic { body, .. } => {
                work.push(body.iter());
            }
            Node::Branch { alts, .. } => {
                // Alternatives are emitted in order: push them last-first.
                work.extend(alts.iter().rev().map(|a| a.iter()));
            }
            Node::GroupRefExists { yes, no, .. } => {
                if let Some(no) = no {
                    work.push(no.iter());
                }
                work.push(yes.iter());
            }
            Node::Literal(_) | Node::Any | Node::Class { .. } | Node::At(_) | Node::GroupRef(_) => {
            }
        }
    }
    if sre_code_too_wide {
        return Err(other_error(ErrorKind::Runtime, "invalid SRE code"));
    }
    Ok(())
}

/// Result of `_escape` / `_class_escape`.
enum Escape {
    Literal(u32),
    Category(Category),
    At(At),
    GroupRef(u32),
}

fn category(c: char) -> Option<Category> {
    Some(match c {
        'd' => Category::Digit,
        'D' => Category::NotDigit,
        's' => Category::Space,
        'S' => Category::NotSpace,
        'w' => Category::Word,
        'W' => Category::NotWord,
        _ => return None,
    })
}

/// `ESCAPES`
fn simple_escape(c: char) -> Option<u32> {
    Some(match c {
        'a' => 0x07,
        'b' => 0x08,
        'f' => 0x0c,
        'n' => 0x0a,
        'r' => 0x0d,
        't' => 0x09,
        'v' => 0x0b,
        '\\' => 0x5c,
        _ => return None,
    })
}

/// `FLAGS`; `t` until Python 3.13.
fn flag_value(t: Tok, minor: u32) -> Option<u32> {
    Some(match t {
        Tok::Ch('t') if minor <= 12 => flags::TEMPLATE,
        Tok::Ch('i') => flags::IGNORECASE,
        Tok::Ch('L') => flags::LOCALE,
        Tok::Ch('m') => flags::MULTILINE,
        Tok::Ch('s') => flags::DOTALL,
        Tok::Ch('x') => flags::VERBOSE,
        Tok::Ch('a') => flags::ASCII,
        Tok::Ch('u') => flags::UNICODE,
        _ => return None,
    })
}

fn tok_isalpha(t: Tok) -> bool {
    matches!(t, Tok::Ch(c) if py_isalpha(c))
}

/// A suspended `_parse` call: its locals, and where its result goes.
struct Frame {
    sub: Vec<Node>,
    verbose: bool,
    nested: usize,
    first: bool,
    ret: Ret,
}

/// Where a finished `_parse` returns to.
enum Ret {
    /// The `_parse_sub` alternation loop that called it.
    Alt(Alt),
    /// `item_yes = _parse(...)` of a conditional.
    CondYes(Cond),
    /// `item_no = _parse(...)` of a conditional, with the `yes` branch.
    CondNo(Cond, Vec<Node>),
}

/// A suspended `_parse_sub` call.
struct Alt {
    items: Vec<Vec<Node>>,
    verbose: bool,
    nested: usize,
    ret: AltRet,
}

/// What the caller of `_parse_sub` does with its result.
enum AltRet {
    Root,
    /// `(?=...)`, `(?!...)`, `(?<=...)`, `(?<!...)`; `reset_lookbehind` when
    /// this look-behind set `state.lookbehindgroups`.
    Look {
        start: usize,
        behind: bool,
        negate: bool,
        reset_lookbehind: bool,
    },
    /// Groups: capturing, `(?:...)`, `(?>...)`, `(?flags:...)`.
    Group {
        start: usize,
        group: Option<u32>,
        atomic: bool,
        add: u32,
        del: u32,
    },
}

/// A conditional `(?(group)yes|no)` awaiting its branches.
struct Cond {
    start: usize,
    group: u64,
    /// `verbose` and `nested` of the `_parse` that contains the conditional.
    verbose: bool,
    nested: usize,
}

impl Frame {
    /// `items.append(_parse(source, state, verbose, nested + 1, not nested and not items))`
    fn alternative(alt: Alt) -> Frame {
        Frame {
            sub: Vec::new(),
            verbose: alt.verbose,
            nested: alt.nested + 1,
            first: alt.nested == 0 && alt.items.is_empty(),
            ret: Ret::Alt(alt),
        }
    }

    /// Python frames this represents (`_parse`, plus `_parse_sub` when that
    /// is the caller).
    fn py_frames(&self) -> usize {
        match self.ret {
            Ret::Alt(_) => 2,
            Ret::CondYes(_) | Ret::CondNo(..) => 1,
        }
    }

    /// The `RecursionError` raised entering this call one level too deep. What exceeds the
    /// limit first on Python 3.10 and 3.11 depends on the parity of upstream's stack depth
    /// where it compiles the pattern, which varies with the document; this is what a direct
    /// `re.compile` call meets: a comparison on 3.10, and on 3.11 a group's `SubPattern()`
    /// call unless reading flags or a condition gets there first.
    fn recursion_message(&self, minor: u32) -> &'static str {
        let plain_group = matches!(
            &self.ret,
            Ret::Alt(Alt {
                ret: AltRet::Look { .. } | AltRet::Group { add: 0, del: 0, .. },
                ..
            })
        );
        match minor {
            ..=10 => "maximum recursion depth exceeded in comparison",
            11 if plain_group => "maximum recursion depth exceeded while calling a Python object",
            _ => "maximum recursion depth exceeded",
        }
    }

    fn dismantle(self) {
        dismantle(self.sub);
        match self.ret {
            Ret::Alt(alt) => alt.items.into_iter().for_each(dismantle),
            Ret::CondNo(_, yes) => dismantle(yes),
            Ret::CondYes(_) => {}
        }
    }
}

struct Parser {
    source: Tokenizer,
    state: State,
    /// The Python minor version whose syntax applies.
    minor: u32,
    /// Whether the whole pattern is verbose from the start (Python 3.10's second pass).
    verbose: bool,
    /// Python 3.10: global `(?x)` turned up after the start; parse again verbosely.
    restart_verbose: bool,
    /// Python frames of `_parse_sub`/`_parse` currently active.
    frames: usize,
    /// Deepest `_parse` nesting reached.
    max_depth: usize,
}

impl Parser {
    fn new(pattern: &str, minor: u32, verbose: bool) -> PResult<Self> {
        Ok(Parser {
            source: Tokenizer::new(pattern)?,
            state: State {
                flags: if verbose { flags::VERBOSE } else { 0 },
                groupdict: Vec::new(),
                groupwidths: vec![None],
                lookbehindgroups: None,
                grouprefpos: Vec::new(),
            },
            minor,
            verbose,
            restart_verbose: false,
            frames: 0,
            max_depth: 0,
        })
    }

    /// `_parse_sub(source, state, verbose, 0)`, releasing what an error leaves behind.
    fn parse_all(&mut self) -> PResult<Vec<Node>> {
        let mut stack = Vec::new();
        let result = self.run(&mut stack);
        for frame in stack {
            frame.dismantle();
        }
        result
    }

    fn width_rules(&self) -> WidthRules {
        if self.minor <= 10 {
            WidthRules::Python310
        } else {
            WidthRules::Current
        }
    }

    fn flag(&self, t: Tok) -> Option<u32> {
        flag_value(t, self.minor)
    }

    /// `MAXGROUPS`: `_sre` allowed twice as many groups in Python 3.10.
    fn maxgroups(&self) -> u64 {
        if self.minor <= 10 {
            (MAXGROUPS << 1) | 1
        } else {
            MAXGROUPS
        }
    }

    /// Enters a `_parse` (and possibly `_parse_sub`) call.
    fn call(&mut self, stack: &mut Vec<Frame>, frame: Frame) -> PResult<()> {
        self.frames += frame.py_frames();
        // Python 3.10 also counts some C calls on upstream's way to `re.compile`.
        let limit = if self.minor <= 10 {
            MAX_PY_FRAMES - 3
        } else {
            MAX_PY_FRAMES
        };
        if self.frames > limit {
            let message = frame.recursion_message(self.minor);
            stack.push(frame);
            return Err(other_error(ErrorKind::Recursion, message));
        }
        stack.push(frame);
        self.max_depth = self.max_depth.max(stack.len());
        Ok(())
    }

    /// `_parse_sub(source, state, verbose, 0)` driven iteratively.
    fn run(&mut self, stack: &mut Vec<Frame>) -> PResult<Vec<Node>> {
        let root = Alt {
            items: Vec::new(),
            verbose: self.verbose,
            nested: 0,
            ret: AltRet::Root,
        };
        self.call(stack, Frame::alternative(root))?;
        loop {
            let Some(frame) = stack.last_mut() else {
                return Ok(Vec::new()); // unreachable: the root returns below
            };
            if let Some(child) = self.parse(frame)? {
                self.call(stack, child)?;
                continue;
            }
            let Some(frame) = stack.pop() else {
                return Ok(Vec::new());
            };
            self.frames -= frame.py_frames();
            let node = match frame.ret {
                Ret::Alt(mut alt) => {
                    alt.items.push(frame.sub);
                    if self.source.match_('|')? {
                        if alt.nested == 0 {
                            alt.verbose = self.state.flags & flags::VERBOSE != 0;
                        }
                        self.call(stack, Frame::alternative(alt))?;
                        continue;
                    }
                    let p = if alt.items.len() == 1 {
                        alt.items.pop().unwrap_or_default()
                    } else {
                        let w =
                            branch_width(&alt.items, &self.state.groupwidths, self.width_rules());
                        vec![Node::Branch { alts: alt.items, w }]
                    };
                    match alt.ret {
                        AltRet::Root => return Ok(p),
                        AltRet::Look {
                            start,
                            behind,
                            negate,
                            reset_lookbehind,
                        } => {
                            if reset_lookbehind {
                                self.state.lookbehindgroups = None;
                            }
                            self.expect_close(start)?;
                            let body_w = width(&p, &self.state.groupwidths, self.width_rules());
                            Node::Look {
                                behind,
                                negate,
                                body: p,
                                body_w,
                            }
                        }
                        AltRet::Group {
                            start,
                            group,
                            atomic,
                            add,
                            del,
                        } => {
                            self.expect_close(start)?;
                            let body_w = width(&p, &self.state.groupwidths, self.width_rules());
                            if let Some(g) = group {
                                // closegroup
                                if let Some(slot) = self.state.groupwidths.get_mut(g as usize) {
                                    *slot = Some(body_w);
                                }
                            }
                            if atomic {
                                Node::Atomic { body: p, body_w }
                            } else {
                                Node::Group {
                                    index: group,
                                    add,
                                    del,
                                    body: p,
                                    body_w,
                                }
                            }
                        }
                    }
                }
                Ret::CondYes(cond) => {
                    if self.source.match_('|')? {
                        let no = Frame {
                            sub: Vec::new(),
                            verbose: cond.verbose,
                            nested: cond.nested + 1,
                            first: false,
                            ret: Ret::CondNo(cond, frame.sub),
                        };
                        self.call(stack, no)?;
                        continue;
                    }
                    self.finish_conditional(cond, frame.sub, None)?
                }
                Ret::CondNo(cond, yes) => {
                    if self.source.next == Some(Tok::Ch('|')) {
                        return Err(self
                            .source
                            .error("conditional backref with more than two branches", 0));
                    }
                    self.finish_conditional(cond, yes, Some(frame.sub))?
                }
            };
            if let Some(parent) = stack.last_mut() {
                parent.sub.push(node);
            }
        }
    }

    fn expect_close(&mut self, start: usize) -> PResult<()> {
        if !self.source.match_(')')? {
            let src = &self.source;
            return Err(src.error("missing ), unterminated subpattern", src.tell() - start));
        }
        Ok(())
    }

    fn finish_conditional(
        &mut self,
        cond: Cond,
        yes: Vec<Node>,
        no: Option<Vec<Node>>,
    ) -> PResult<Node> {
        self.expect_close(cond.start)?;
        let gw = &self.state.groupwidths;
        let rules = self.width_rules();
        let (i, j) = width(&yes, gw, rules);
        let w = match &no {
            Some(no) => {
                let (l, h) = width(no, gw, rules);
                (i.min(l), j.max(h))
            }
            None => (0, j),
        };
        Ok(Node::GroupRefExists {
            group: cond.group as u32,
            yes,
            no,
            w,
        })
    }

    /// The body of `_parse`'s loop for `frame`. Returns `Some(child)` when a
    /// nested `_parse`/`_parse_sub` call is needed (the frame resumes when it
    /// returns), `None` when this `_parse` returns.
    fn parse(&mut self, frame: &mut Frame) -> PResult<Option<Frame>> {
        loop {
            let Some(this) = self.source.next else {
                return Ok(None); // end of pattern
            };
            if this.is('|') || this.is(')') {
                return Ok(None); // end of subpattern
            }
            self.source.get()?;

            if frame.verbose {
                // skip whitespace and comments
                if this.ch_in(WHITESPACE) {
                    continue;
                }
                if this.is('#') {
                    loop {
                        match self.source.get()? {
                            None => break,
                            Some(t) if t.is('\n') => break,
                            Some(_) => {}
                        }
                    }
                    continue;
                }
            }

            let c = match this {
                Tok::Esc(c) => {
                    let node = match self.escape(c)? {
                        Escape::Literal(v) => Node::Literal(v),
                        Escape::Category(cat) => Node::Class {
                            negate: false,
                            items: vec![ClassItem::Category(cat)],
                        },
                        Escape::At(at) => Node::At(at),
                        Escape::GroupRef(g) => Node::GroupRef(g),
                    };
                    frame.sub.push(node);
                    continue;
                }
                Tok::Ch(c) => c,
            };
            if !SPECIAL_CHARS.contains(c) {
                frame.sub.push(Node::Literal(c as u32));
                continue;
            }
            match c {
                '[' => {
                    let node = self.parse_class()?;
                    frame.sub.push(node);
                }
                '*' | '+' | '?' | '{' => self.parse_repeat(c, &mut frame.sub)?,
                '.' => frame.sub.push(Node::Any),
                '(' => {
                    if let Some(child) = self.parse_group(frame)? {
                        return Ok(Some(child));
                    }
                }
                '^' => frame.sub.push(Node::At(At::Beginning)),
                '$' => frame.sub.push(Node::At(At::End)),
                // `|` and `)` were handled above; SPECIAL_CHARS has nothing else.
                _ => frame.sub.push(Node::Literal(c as u32)),
            }
        }
    }

    /// The `\x`, `\u`, `\U` and `\N` branches shared by `_escape` and
    /// `_class_escape`. Returns `None` when `c` is not one of them.
    fn unicode_escape(&mut self, c: char) -> PResult<Option<u32>> {
        let src = &mut self.source;
        let digits = match c {
            'x' => 2,
            'u' => 4,
            'U' => 8,
            'N' => {
                // named unicode escape e.g. \N{EM DASH}
                if !src.match_('{')? {
                    return Err(src.error("missing {", 0));
                }
                let (charname, len) = src.getuntil('}', "character name")?;
                return match lookup_name(&charname) {
                    Some(cp) => Ok(Some(cp)),
                    None => Err(src.error(
                        format!("undefined character name {}", py_repr(&charname)),
                        len + 4,
                    )),
                };
            }
            _ => return Ok(None),
        };
        let hex = src.getwhile(digits, |c| c.is_ascii_hexdigit())?;
        let escape = format!("\\{c}{hex}");
        if hex.len() != digits {
            return Err(src.error(format!("incomplete escape {escape}"), escape.len()));
        }
        let value = hex_value(&hex);
        if value > 0x7FFF_FFFF && self.minor <= 12 {
            // `chr(c)` took a C int until Python 3.13.
            return Err(other_error(
                ErrorKind::Overflow,
                "Python int too large to convert to C int",
            ));
        }
        if value > 0x10FFFF {
            // chr(c) raises ValueError -> "bad escape"
            return Err(src.error(format!("bad escape {escape}"), escape.len()));
        }
        Ok(Some(value))
    }

    /// `_class_escape`
    fn class_escape(&mut self, c: char) -> PResult<ClassItem> {
        if let Some(v) = simple_escape(c) {
            return Ok(ClassItem::Literal(v));
        }
        if let Some(cat) = category(c) {
            return Ok(ClassItem::Category(cat));
        }
        if let Some(v) = self.unicode_escape(c)? {
            return Ok(ClassItem::Literal(v));
        }
        let src = &mut self.source;
        if is_oct(c) {
            // octal escape (up to three digits)
            let more = src.getwhile(2, is_oct)?;
            let escape = format!("\\{c}{more}");
            let value = oct_value(&escape[1..]);
            if value > 0o377 {
                return Err(src.error(
                    format!("octal escape value {escape} outside of range 0-0o377"),
                    escape.len(),
                ));
            }
            return Ok(ClassItem::Literal(value));
        }
        if c.is_ascii_alphanumeric() {
            // unknown ASCII letters, and `\8`/`\9` (ValueError)
            return Err(src.error(format!("bad escape \\{c}"), 2));
        }
        Ok(ClassItem::Literal(c as u32))
    }

    /// `_escape`
    fn escape(&mut self, c: char) -> PResult<Escape> {
        match c {
            'A' => return Ok(Escape::At(At::BeginningString)),
            'b' => return Ok(Escape::At(At::Boundary)),
            'B' => return Ok(Escape::At(At::NonBoundary)),
            'Z' => return Ok(Escape::At(At::EndString)),
            'z' if self.minor >= 14 => return Ok(Escape::At(At::EndString)),
            _ => {}
        }
        if let Some(cat) = category(c) {
            return Ok(Escape::Category(cat));
        }
        if let Some(v) = simple_escape(c) {
            return Ok(Escape::Literal(v));
        }
        if let Some(v) = self.unicode_escape(c)? {
            return Ok(Escape::Literal(v));
        }
        if c == '0' {
            // octal escape
            let more = self.source.getwhile(2, is_oct)?;
            return Ok(Escape::Literal(oct_value(&more)));
        }
        if c.is_ascii_digit() {
            // octal escape *or* decimal group reference (sigh)
            let mut escape = format!("\\{c}");
            if let Some(d) = self.source.next.and_then(Tok::digit) {
                self.source.get()?;
                escape.push(d);
                if is_oct(c)
                    && is_oct(d)
                    && matches!(self.source.next, Some(Tok::Ch(n)) if is_oct(n))
                {
                    // got three octal digits; this is an octal escape
                    if let Some(Tok::Ch(n)) = self.source.get()? {
                        escape.push(n);
                    }
                    let value = oct_value(&escape[1..]);
                    if value > 0o377 {
                        return Err(self.source.error(
                            format!("octal escape value {escape} outside of range 0-0o377"),
                            escape.len(),
                        ));
                    }
                    return Ok(Escape::Literal(value));
                }
            }
            // not an octal escape, so this is a group reference
            let group: u64 = escape[1..].parse().unwrap_or(0);
            if (group as usize) < self.state.groups() {
                if !self.state.checkgroup(group) {
                    return Err(self
                        .source
                        .error("cannot refer to an open group", escape.len()));
                }
                self.state.checklookbehindgroup(group, &self.source)?;
                return Ok(Escape::GroupRef(group as u32));
            }
            return Err(self
                .source
                .error(format!("invalid group reference {group}"), escape.len() - 1));
        }
        if c.is_ascii_alphabetic() {
            return Err(self.source.error(format!("bad escape \\{c}"), 2));
        }
        Ok(Escape::Literal(c as u32))
    }

    /// The `[` branch of `_parse`.
    fn parse_class(&mut self) -> PResult<Node> {
        let here = self.source.tell() - 1;
        let negate = self.source.match_('^')?;
        let mut items: Vec<ClassItem> = Vec::new();
        let unterminated =
            |src: &Tokenizer| src.error("unterminated character set", src.tell() - here);
        loop {
            let Some(this) = self.source.get()? else {
                return Err(unterminated(&self.source));
            };
            if this.is(']') && !items.is_empty() {
                break;
            }
            let code1 = match this {
                Tok::Esc(c) => self.class_escape(c)?,
                Tok::Ch(c) => ClassItem::Literal(c as u32),
            };
            if !self.source.match_('-')? {
                items.push(code1);
                continue;
            }
            // potential range
            let Some(that) = self.source.get()? else {
                return Err(unterminated(&self.source));
            };
            if that.is(']') {
                items.push(code1);
                items.push(ClassItem::Literal('-' as u32));
                break;
            }
            let code2 = match that {
                Tok::Esc(c) => self.class_escape(c)?,
                Tok::Ch(c) => ClassItem::Literal(c as u32),
            };
            match (code1, code2) {
                (ClassItem::Literal(lo), ClassItem::Literal(hi)) if hi >= lo => {
                    items.push(ClassItem::Range(lo, hi));
                }
                _ => {
                    return Err(self.source.error(
                        format!("bad character range {}-{}", this.text(), that.text()),
                        this.len() + 1 + that.len(),
                    ));
                }
            }
        }
        Ok(Node::Class { negate, items })
    }

    /// The repeat branch of `_parse`: replaces the last item of `sub` (or
    /// pushes a literal `{`).
    fn parse_repeat(&mut self, this: char, sub: &mut Vec<Node>) -> PResult<()> {
        let rules = self.width_rules();
        let possessive = self.minor >= 11;
        let src = &mut self.source;
        let here = src.tell();
        let (min, max) = match this {
            '?' => (0, 1),
            '*' => (0, MAXREPEAT),
            '+' => (1, MAXREPEAT),
            _ => {
                if src.next == Some(Tok::Ch('}')) {
                    sub.push(Node::Literal('{' as u32));
                    return Ok(());
                }
                let mut lo = String::new();
                let mut hi = String::new();
                while let Some(d) = src.next.and_then(Tok::digit) {
                    src.get()?;
                    lo.push(d);
                }
                if src.match_(',')? {
                    while let Some(d) = src.next.and_then(Tok::digit) {
                        src.get()?;
                        hi.push(d);
                    }
                } else {
                    hi.clone_from(&lo);
                }
                if !src.match_('}')? {
                    sub.push(Node::Literal('{' as u32));
                    return src.seek(here);
                }
                let too_large =
                    || other_error(ErrorKind::Overflow, "the repetition number is too large");
                let mut min = 0;
                let mut max = MAXREPEAT;
                if !lo.is_empty() {
                    let v = py_int(&lo)?;
                    if v >= u64::from(MAXREPEAT) {
                        return Err(too_large());
                    }
                    min = v as u32;
                }
                if !hi.is_empty() {
                    let v = py_int(&hi)?;
                    if v >= u64::from(MAXREPEAT) {
                        return Err(too_large());
                    }
                    max = v as u32;
                    if max < min {
                        return Err(
                            src.error("min repeat greater than max repeat", src.tell() - here)
                        );
                    }
                }
                (min, max)
            }
        };
        // figure out which item to repeat
        let offset = src.tell() - here + 1;
        let (body, body_w) = match sub.pop() {
            None | Some(Node::At(_)) => return Err(src.error("nothing to repeat", offset)),
            Some(Node::Repeat { .. }) => return Err(src.error("multiple repeat", offset)),
            Some(Node::Group {
                index: None,
                add: 0,
                del: 0,
                body,
                body_w,
            }) => (body, body_w),
            Some(node) => {
                let item = vec![node];
                let w = width(&item, &self.state.groupwidths, rules);
                (item, w)
            }
        };
        let kind = if src.match_('?')? {
            RepeatKind::Lazy
        } else if possessive && src.match_('+')? {
            RepeatKind::Possessive
        } else {
            RepeatKind::Greedy
        };
        sub.push(Node::Repeat {
            kind,
            min,
            max,
            body,
            body_w,
        });
        Ok(())
    }

    /// The `(` branch of `_parse`. Returns the frame of the nested call for
    /// groups, look-arounds and conditionals; handles the rest inline.
    fn parse_group(&mut self, frame: &mut Frame) -> PResult<Option<Frame>> {
        let start = self.source.tell() - 1;
        let mut capture = true;
        let mut atomic = false;
        let mut name: Option<(String, usize)> = None;
        let mut add_flags = 0;
        let mut del_flags = 0;
        if self.source.match_('?')? {
            // options
            let Some(ch) = self.source.get()? else {
                return Err(self.source.error("unexpected end of pattern", 0));
            };
            match ch {
                Tok::Ch('P') => {
                    // python extensions
                    if self.source.match_('<')? {
                        // named group: skip forward to end of name
                        let (n, len) = self.source.getuntil('>', "group name")?;
                        self.source.checkgroupname(&n, len, 1)?;
                        name = Some((n, len));
                    } else if self.source.match_('=')? {
                        // named backreference
                        let (n, len) = self.source.getuntil(')', "group name")?;
                        self.source.checkgroupname(&n, len, 1)?;
                        let Some(gid) = self.state.group_index(&n) else {
                            return Err(self
                                .source
                                .error(format!("unknown group name {}", py_repr(&n)), len + 1));
                        };
                        if !self.state.checkgroup(u64::from(gid)) {
                            return Err(self
                                .source
                                .error("cannot refer to an open group", len + 1));
                        }
                        self.state
                            .checklookbehindgroup(u64::from(gid), &self.source)?;
                        frame.sub.push(Node::GroupRef(gid));
                        return Ok(None);
                    } else {
                        let Some(ch) = self.source.get()? else {
                            return Err(self.source.error("unexpected end of pattern", 0));
                        };
                        return Err(self
                            .source
                            .error(format!("unknown extension ?P{}", ch.text()), ch.len() + 2));
                    }
                }
                Tok::Ch(':') => {
                    // non-capturing group
                    capture = false;
                }
                Tok::Ch('#') => {
                    // comment
                    loop {
                        if self.source.next.is_none() {
                            let src = &self.source;
                            return Err(
                                src.error("missing ), unterminated comment", src.tell() - start)
                            );
                        }
                        if self.source.get()? == Some(Tok::Ch(')')) {
                            break;
                        }
                    }
                    return Ok(None);
                }
                Tok::Ch(c @ ('=' | '!' | '<')) => {
                    // lookahead assertions
                    let mut c = c;
                    let mut behind = false;
                    let mut reset_lookbehind = false;
                    if c == '<' {
                        let Some(ch) = self.source.get()? else {
                            return Err(self.source.error("unexpected end of pattern", 0));
                        };
                        match ch {
                            Tok::Ch(d @ ('=' | '!')) => c = d,
                            _ => {
                                return Err(self.source.error(
                                    format!("unknown extension ?<{}", ch.text()),
                                    ch.len() + 2,
                                ));
                            }
                        }
                        behind = true; // lookbehind
                        if self.state.lookbehindgroups.is_none() {
                            self.state.lookbehindgroups = Some(self.state.groups());
                            reset_lookbehind = true;
                        }
                    }
                    let look = AltRet::Look {
                        start,
                        behind,
                        negate: c == '!',
                        reset_lookbehind,
                    };
                    return Ok(Some(Frame::alternative(Alt {
                        items: Vec::new(),
                        verbose: frame.verbose,
                        nested: frame.nested + 1,
                        ret: look,
                    })));
                }
                Tok::Ch('(') => {
                    // conditional backreference group
                    let group = self.parse_condition()?;
                    let cond = Cond {
                        start,
                        group,
                        verbose: frame.verbose,
                        nested: frame.nested,
                    };
                    return Ok(Some(Frame {
                        sub: Vec::new(),
                        verbose: frame.verbose,
                        nested: frame.nested + 1,
                        first: false,
                        ret: Ret::CondYes(cond),
                    }));
                }
                Tok::Ch('>') if self.minor >= 11 => {
                    // non-capturing, atomic group
                    capture = false;
                    atomic = true;
                }
                ch if self.flag(ch).is_some() || ch.is('-') => {
                    // flags
                    match self.parse_flags(ch)? {
                        None if self.minor <= 10 => {
                            // global flags, anywhere (deprecated); they apply to the
                            // whole pattern
                            if self.state.flags & flags::VERBOSE != 0 && !frame.verbose {
                                self.restart_verbose = true;
                                return Err(other_error(ErrorKind::Re, "verbose"));
                            }
                            return Ok(None);
                        }
                        None => {
                            // global flags
                            if !frame.first || !frame.sub.is_empty() {
                                let src = &self.source;
                                return Err(src.error(
                                    "global flags not at the start of the expression",
                                    src.tell() - start,
                                ));
                            }
                            frame.verbose = self.state.flags & flags::VERBOSE != 0;
                            return Ok(None);
                        }
                        Some((add, del)) => {
                            add_flags = add;
                            del_flags = del;
                            capture = false;
                        }
                    }
                }
                ch => {
                    return Err(self
                        .source
                        .error(format!("unknown extension ?{}", ch.text()), ch.len() + 1));
                }
            }
        }

        // parse group contents
        let group = if capture {
            Some(self.opengroup(name.as_ref())?)
        } else {
            None
        };
        let sub_verbose =
            (frame.verbose || add_flags & flags::VERBOSE != 0) && del_flags & flags::VERBOSE == 0;
        Ok(Some(Frame::alternative(Alt {
            items: Vec::new(),
            verbose: sub_verbose,
            nested: frame.nested + 1,
            ret: AltRet::Group {
                start,
                group,
                atomic,
                add: add_flags,
                del: del_flags,
            },
        })))
    }

    /// `State.opengroup`, with the re-raise from `_parse`.
    fn opengroup(&mut self, name: Option<&(String, usize)>) -> PResult<u32> {
        let gid = self.state.groups();
        self.state.groupwidths.push(None);
        if self.state.groups() as u64 > self.maxgroups() {
            return Err(match name {
                Some((_, len)) => self.source.error("too many groups", len + 1),
                // `len(None)` in the error handler
                None => other_error(ErrorKind::Type, "object of type 'NoneType' has no len()"),
            });
        }
        if let Some((n, len)) = name {
            if let Some(ogid) = self.state.group_index(n) {
                return Err(self.source.error(
                    format!(
                        "redefinition of group name {} as group {gid}; was group {ogid}",
                        py_repr(n)
                    ),
                    len + 1,
                ));
            }
            self.state.groupdict.push((n.clone(), gid as u32));
        }
        Ok(gid as u32)
    }

    /// The condition of `(?(...)`: returns the referenced group.
    fn parse_condition(&mut self) -> PResult<u64> {
        let (condname, len) = self.source.getuntil(')', "group name")?;
        // Until Python 3.12, a name that is not an identifier is read with `int()`.
        let number = if self.minor <= 11 && !is_identifier(&condname) {
            match py_int_text(&condname) {
                Some(digits) if !digits.starts_with('-') => Some(digits),
                _ => {
                    return Err(self.source.error(
                        format!("bad character in group name {}", py_repr(&condname)),
                        len + 1,
                    ));
                }
            }
        } else {
            condname
                .bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| condname.clone())
        };
        let condgroup = if let Some(condname) = number {
            let g = py_int(&condname)?;
            if g == 0 {
                return Err(self.source.error("bad group number", len + 1));
            }
            if g >= self.maxgroups() {
                return Err(self.source.error(
                    format!("invalid group reference {}", int_text(&condname)),
                    len + 1,
                ));
            }
            if !self.state.grouprefpos.iter().any(|&(x, _)| x == g) {
                let pos = self.source.tell() - len - 1;
                self.state.grouprefpos.push((g, pos));
            }
            g
        } else {
            self.source.checkgroupname(&condname, len, 1)?;
            let Some(g) = self.state.group_index(&condname) else {
                return Err(self.source.error(
                    format!("unknown group name {}", py_repr(&condname)),
                    len + 1,
                ));
            };
            u64::from(g)
        };
        self.state.checklookbehindgroup(condgroup, &self.source)?;
        Ok(condgroup)
    }

    /// `_parse_flags`; `None` means global flags (`(?x)`).
    fn parse_flags(&mut self, mut ch: Tok) -> PResult<Option<(u32, u32)>> {
        let mut add_flags = 0;
        let mut del_flags = 0;
        if !ch.is('-') {
            loop {
                let flag = self.flag(ch).unwrap_or(0);
                if ch.is('L') {
                    return Err(self.source.error(
                        "bad inline flags: cannot use 'L' flag with a str pattern",
                        0,
                    ));
                }
                add_flags |= flag;
                if flag & flags::TYPE_FLAGS != 0 && add_flags & flags::TYPE_FLAGS != flag {
                    return Err(self.source.error(
                        "bad inline flags: flags 'a', 'u' and 'L' are incompatible",
                        0,
                    ));
                }
                let Some(next) = self.source.get()? else {
                    return Err(self.source.error("missing -, : or )", 0));
                };
                ch = next;
                if ch.ch_in(")-:") {
                    break;
                }
                if self.flag(ch).is_none() {
                    let msg = if tok_isalpha(ch) {
                        "unknown flag"
                    } else {
                        "missing -, : or )"
                    };
                    return Err(self.source.error(msg, ch.len()));
                }
            }
        }
        if ch.is(')') {
            self.state.flags |= add_flags;
            return Ok(None);
        }
        if add_flags & flags::TEMPLATE != 0 {
            return Err(self
                .source
                .error("bad inline flags: cannot turn on global flag", 1));
        }
        if ch.is('-') {
            let Some(next) = self.source.get()? else {
                return Err(self.source.error("missing flag", 0));
            };
            ch = next;
            if self.flag(ch).is_none() {
                let msg = if tok_isalpha(ch) {
                    "unknown flag"
                } else {
                    "missing flag"
                };
                return Err(self.source.error(msg, ch.len()));
            }
            loop {
                let flag = self.flag(ch).unwrap_or(0);
                if flag & flags::TYPE_FLAGS != 0 {
                    return Err(self.source.error(
                        "bad inline flags: cannot turn off flags 'a', 'u' and 'L'",
                        0,
                    ));
                }
                del_flags |= flag;
                let Some(next) = self.source.get()? else {
                    return Err(self.source.error("missing :", 0));
                };
                ch = next;
                if ch.is(':') {
                    break;
                }
                if self.flag(ch).is_none() {
                    let msg = if tok_isalpha(ch) {
                        "unknown flag"
                    } else {
                        "missing :"
                    };
                    return Err(self.source.error(msg, ch.len()));
                }
            }
        }
        if del_flags & flags::TEMPLATE != 0 {
            return Err(self
                .source
                .error("bad inline flags: cannot turn off global flag", 1));
        }
        if add_flags & del_flags != 0 {
            return Err(self
                .source
                .error("bad inline flags: flag turned on and off", 1));
        }
        Ok(Some((add_flags, del_flags)))
    }
}
