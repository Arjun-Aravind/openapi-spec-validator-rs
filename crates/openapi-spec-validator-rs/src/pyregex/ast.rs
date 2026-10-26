//! Intermediate representation produced by the parser.
//!
//! Mirrors the `(opcode, argument)` tuples of CPython's `re._parser.SubPattern`
//! closely enough to reproduce `getwidth()` and to translate for matching.
//! Container nodes cache the width of their contents so that computing widths
//! never recurses (patterns may nest hundreds of levels deep).

/// `SRE_FLAG_*` values from `re/_constants.py`.
pub(crate) mod flags {
    /// `(?t)`, accepted until Python 3.13 removed it.
    pub const TEMPLATE: u32 = 1;
    pub const IGNORECASE: u32 = 2;
    pub const LOCALE: u32 = 4;
    pub const MULTILINE: u32 = 8;
    pub const DOTALL: u32 = 16;
    pub const UNICODE: u32 = 32;
    pub const VERBOSE: u32 = 64;
    pub const ASCII: u32 = 256;
    /// `TYPE_FLAGS` in `_parser.py`.
    pub const TYPE_FLAGS: u32 = ASCII | LOCALE | UNICODE;

    /// `_compiler._combine_flags`.
    pub fn combine(flags: u32, add: u32, del: u32) -> u32 {
        let base = if add & TYPE_FLAGS != 0 {
            flags & !TYPE_FLAGS
        } else {
            flags
        };
        (base | add) & !del
    }
}

/// `_sre.MAXREPEAT`: also the sentinel for an unbounded repeat.
pub(crate) const MAXREPEAT: u32 = u32::MAX;
/// `_sre.MAXGROUPS` on 64-bit builds.
pub(crate) const MAXGROUPS: u64 = 1_073_741_823;
/// `_parser.MAXWIDTH`.
pub(crate) const MAXWIDTH: u128 = 1 << 64;
/// `_compiler.MAXCODE` (`_sre.CODESIZE == 4`).
pub(crate) const MAXCODE: u128 = (1 << 32) - 1;

/// `(lo, hi)` as returned by `SubPattern.getwidth()`.
pub(crate) type Width = (u128, u128);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Category {
    Digit,
    NotDigit,
    Space,
    NotSpace,
    Word,
    NotWord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum At {
    /// `^`
    Beginning,
    /// `\A`
    BeginningString,
    /// `\b`
    Boundary,
    /// `\B`
    NonBoundary,
    /// `$`
    End,
    /// `\Z`
    EndString,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClassItem {
    Literal(u32),
    Range(u32, u32),
    Category(Category),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepeatKind {
    Greedy,
    Lazy,
    Possessive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Node {
    /// `LITERAL` (a code point; may be a lone surrogate).
    Literal(u32),
    /// `ANY`
    Any,
    /// `IN` (also covers `NOT_LITERAL`, i.e. `[^x]`).
    Class { negate: bool, items: Vec<ClassItem> },
    /// `AT`
    At(At),
    /// `MAX_REPEAT` / `MIN_REPEAT` / `POSSESSIVE_REPEAT`; `max == MAXREPEAT` is
    /// unbounded. `body_w` is the width of `body`.
    Repeat {
        kind: RepeatKind,
        min: u32,
        max: u32,
        body: Vec<Node>,
        body_w: Width,
    },
    /// `SUBPATTERN`: capturing group (`index`) or scoped-flags / plain
    /// non-capturing group.
    Group {
        index: Option<u32>,
        add: u32,
        del: u32,
        body: Vec<Node>,
        body_w: Width,
    },
    /// `ATOMIC_GROUP`
    Atomic { body: Vec<Node>, body_w: Width },
    /// `ASSERT` / `ASSERT_NOT` (an empty negative body is `FAILURE`).
    Look {
        behind: bool,
        negate: bool,
        body: Vec<Node>,
        body_w: Width,
    },
    /// `BRANCH`; `w` is the width of the whole branch.
    Branch { alts: Vec<Vec<Node>>, w: Width },
    /// `GROUPREF`
    GroupRef(u32),
    /// `GROUPREF_EXISTS`; `w` is the width of the whole node.
    GroupRefExists {
        group: u32,
        yes: Vec<Node>,
        no: Option<Vec<Node>>,
        w: Width,
    },
}

/// The bounds of `SubPattern.getwidth()`: `(MAXWIDTH, MAXWIDTH)` from Python 3.11, and
/// `(MAXREPEAT - 1, MAXREPEAT)` before, when an unbounded repeat also added only
/// `MAXREPEAT` times its item's width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WidthRules {
    Current,
    Python310,
}

impl WidthRules {
    fn clamp(self, (lo, hi): Width) -> Width {
        match self {
            WidthRules::Current => (lo.min(MAXWIDTH), hi.min(MAXWIDTH)),
            WidthRules::Python310 => (
                lo.min(u128::from(MAXREPEAT) - 1),
                hi.min(u128::from(MAXREPEAT)),
            ),
        }
    }
}

pub(crate) fn width(seq: &[Node], groupwidths: &[Option<Width>], rules: WidthRules) -> Width {
    seq.iter().fold((0, 0), |(lo, hi), node| {
        let (i, j) = match node {
            Node::Literal(_) | Node::Any | Node::Class { .. } => (1, 1),
            Node::At(_) | Node::Look { .. } => (0, 0),
            Node::Repeat {
                min, max, body_w, ..
            } => {
                let (i, j) = *body_w;
                let rlo = i.saturating_mul(u128::from(*min));
                let rhi = if *max == MAXREPEAT && j != 0 && rules == WidthRules::Current {
                    MAXWIDTH
                } else {
                    j.saturating_mul(u128::from(*max))
                };
                (rlo, rhi)
            }
            Node::Group { body_w, .. } | Node::Atomic { body_w, .. } => *body_w,
            Node::Branch { w, .. } | Node::GroupRefExists { w, .. } => *w,
            Node::GroupRef(g) => groupwidths
                .get(*g as usize)
                .copied()
                .flatten()
                .unwrap_or((0, 0)),
        };
        rules.clamp((lo.saturating_add(i), hi.saturating_add(j)))
    })
}

pub(crate) fn branch_width(
    alts: &[Vec<Node>],
    groupwidths: &[Option<Width>],
    rules: WidthRules,
) -> Width {
    alts.iter().fold((MAXWIDTH, 0), |(i, j), alt| {
        let (l, h) = width(alt, groupwidths, rules);
        (i.min(l), j.max(h))
    })
}

pub(crate) fn dismantle(mut work: Vec<Node>) {
    while let Some(node) = work.pop() {
        match node {
            Node::Repeat { body, .. }
            | Node::Group { body, .. }
            | Node::Atomic { body, .. }
            | Node::Look { body, .. } => work.extend(body),
            Node::Branch { alts, .. } => alts.into_iter().for_each(|a| work.extend(a)),
            Node::GroupRefExists { yes, no, .. } => {
                work.extend(yes);
                work.extend(no.into_iter().flatten());
            }
            Node::Literal(_) | Node::Any | Node::Class { .. } | Node::At(_) | Node::GroupRef(_) => {
            }
        }
    }
}
