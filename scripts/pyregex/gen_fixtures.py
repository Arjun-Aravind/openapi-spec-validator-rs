"""Generate differential fixtures for the Rust `pyregex` module from CPython's `re`.

Run with the reference interpreter (CPython 3.13):
    python -I scripts/pyregex/gen_fixtures.py <spec.json>...

Writes tests/fixtures/{validity,matching,classes,casefold,chars}.json (or to
$PYREGEX_FIXTURES; $PYREGEX_SEED and $PYREGEX_SCALE vary the random fuzz).
"""

import json
import os
import random
import re
import sys
import warnings
from re import _constants as C
from re import _parser

warnings.simplefilter("ignore")

assert sys.version_info[:2] == (3, 13), sys.version
HERE = os.path.dirname(os.path.abspath(__file__))
# Optional overrides for large one-off runs: output dir, RNG seed, fuzz scale.
OUT = os.environ.get("PYREGEX_FIXTURES") or os.path.join(
    HERE, "..", "..", "crates", "openapi-spec-validator-rs", "tests", "fixtures", "pyregex"
)
SCALE = int(os.environ.get("PYREGEX_SCALE", "1"))
rng = random.Random(int(os.environ.get("PYREGEX_SEED", "20261023")))


# openapi-spec-validator 0.9.0 calls re.compile 18 Python frames deeper than a
# direct call from main() would (measured: 486 nested groups raise
# RecursionError there, 495 here). classify() -> at_depth(16..0) -> _classify()
# adds those 18 frames, so the fixture's RecursionError boundary is upstream's.
UPSTREAM_EXTRA_FRAMES = 16


def at_depth(k, f, *args):
    return f(*args) if k == 0 else at_depth(k - 1, f, *args)


def classify(p):
    return at_depth(UPSTREAM_EXTRA_FRAMES, _classify, p)


def _classify(p):
    """Outcome of re.compile(p)."""
    try:
        re.compile(p)
        return {"kind": "ok"}
    except re.error as e:
        return {"kind": "re", "msg": e.msg, "pos": e.pos, "str": str(e)}
    except OverflowError as e:
        return {"kind": "overflow", "msg": str(e), "pos": None, "str": str(e)}
    except RecursionError as e:
        return {"kind": "recursion", "msg": str(e), "pos": None, "str": str(e)}
    except ValueError as e:
        return {"kind": "value", "msg": str(e), "pos": None, "str": str(e)}
    except TypeError as e:
        return {"kind": "type", "msg": str(e), "pos": None, "str": str(e)}


def no_surrogates(s):
    return not any(0xD800 <= ord(c) <= 0xDFFF for c in s)


# --------------------------------------------------------------------------
# Hand-written edge cases (one or more per `_parser.py` error branch)
# --------------------------------------------------------------------------
HAND = r"""
a
\
a\
[a\
(?P<a\
abc\
[
[]
[]]
[^]
[^]]
[]a]
[a-]
[-a]
[a-\]
[a-
[z-a]
[a-a]
[\d-z]
[a-\d]
[\w-]
[\w]
[\W\d]
[^\W\d_]
[\b]
[\B]
[\A]
[\Z]
[\z]
[\8]
[\9]
[\0]
[\07]
[\377]
[\400]
[\777]
[\x41]
[\x4]
[\x]
[\u0041]
[\u004]
[\U00000041]
[\U0010FFFF]
[\U00110000]
[\N{EM DASH}]
[\N{em dash}]
[\N]
[\N{]
[\N{}]
[\N{EM DASH]
[\N{NOT A REAL NAME!}]
[\N{LATIN SMALL LETTER R WITH TILDE}]
[\q]
[\-]
[\]]
[\\]
[\/]
[\é]
[[]
[[a]]
[a--b]
[a&&b]
[a~~b]
[a||b]
[--a]
\d
\D
\s
\S
\w
\W
\b
\B
\A
\Z
\z
\G
\e
\c
\cA
\k<x>
\k
\p{L}
\P{L}
\pL
\h
\H
\R
\X
\K
\Q
\E
\i
\j
\l
\m
\o
\q
\y
\g<1>
\g
\0
\00
\000
\0000
\07
\08
\1
\2
\9
\10
\11
\12
\17
\18
\19
\77
\100
\177
\377
\400
\477
\777
\8
\80
(a)\1
(a)\2
(a)\10
(a)\11
(a)(b)(c)(d)(e)(f)(g)(h)(i)(j)\10
(a)(b)(c)(d)(e)(f)(g)(h)(i)(j)\11
(a\1)
(a)(?P=a)
(?P<a>a)(?P=a)
(?P<a>a(?P=a))
(?P<a>a)(?P=b)
(?P<a>a)(?P=)
(?P<a>a)(?P=a
(?P<a>a)(?P=1)
(?P=a)
(?P<>a)
(?P<1>a)
(?P<a1>a)
(?P<_>a)
(?P<a-b>a)
(?P<a b>a)
(?P<é>a)
(?P<名前>a)
(?P<ⅷ>a)
(?P<a·b>a)
(?P<·>a)
(?P<a>a)(?P<a>b)
(?P<a>a)|(?P<a>b)
(?P<a
(?P<a>
(?P<a>a
(?P
(?P>a)
(?Px
(?P\<a>x)
(?<a>x)
(?<=a)
(?<!a)
(?<
(?<x
(?<\=a)
(?=a)
(?!a)
(?!)
(?=)
(?<=)
(?<!)
(?
(?)
(?:
(?:)
(?:a
(?:a|b
(?:a|b)
(?#comment)
(?#comment
(?#comm\)ent)
(?#
a(?#x)*
(?#x)*
(?#x)a*
(?>a)
(?>a+)b
(?>
a*+
a++
a?+
a{1,2}+
a*++
a**
a*?*
a*??
a+*
a??
a{2}{3}
a{2}*
(?:a*)*
(?:a{2}){3}
(?:)*
(?:^)*
(?:$)+
(?:\b)?
(?=a)*
(?!a)+
(?<=a)?
^*
$*
\b*
\B+
\A?
\Z{2}
*
+
?
{1}
{1,2}
{
{}
{,}
a{
a{}
a{,}
a{,5}
a{5,}
a{5}
a{5,3}
a{3,5}
a{0}
a{0,0}
a{1,1}
a{01}
a{ 1}
a{1 }
a{1, 2}
a{1,2
a{1,2}}
a{a}
a{1a}
a{-1}
a{4294967294}
a{4294967295}
a{4294967296}
a{0,4294967294}
a{0,4294967295}
a{99999999999999999999}
a{5,99999999999999999999}
a{99999999999999999999,5}
a{4294967295,1}
a{3,99999999999999999999}
x{2,1}\
)
a)
())
(()
((a)
((a)|b
a|
|a
|
||
a||b
(a|b|c)
(?:a|b|c)
(?i)
(?i)a
a(?i)
(?i)(?m)a
(?i)(?m)(?s)(?x)a
(?im)a
(?imsx)a
(?L)a
(?a)a
(?u)a
(?au)a
(?ua)a
(?aL)a
(?a)(?u)a
(?u)(?a)a
(?a)(?a)a
(?i:a)
(?i-m:a)
(?-i:a)
(?-:a)
(?-)
(?i-)
(?i-i:a)
(?i-x:a)
(?a:a)
(?u:a)
(?L:a)
(?-a:a)
(?-u:a)
(?-L:a)
(?ix-ms:a)
(?i-ms
(?i-
(?i
(?i:
(?im
(?q)
(?iq)
(?i-q:a)
(?-q:a)
(?i1)
(?i-1:a)
(?-1:a)
(?iÉ)
(?i-É:a)
(?i\x)
(?i-\x:a)
(?-\x:a)
(?x) a b # comment
(?x)a b c
(?x)a\ b
(?x)[ ]
(?x)a # comment (unbalanced
(?x)a{1, 2}
(?x)(?#c)a
(?x: a b )c
(?-x: a b )c
(?x)a|(?-x: b)
a|(?i)b
(a)(?i)
 (?i)a
(?#c)(?i)a
(?:)(?i)a
(?x) (?i)a
(?x)
(?x)#
(?x)#(?i)
(?x)\#(?i)
(?x)a(?i)
(?s).
(?m)^$
(?(1)a|b)
(a)(?(1)a|b)
(a)(?(1)a|b|c)
(a)(?(1)a)
(a)(?(1))
(a)(?(1)|b)
(?(1)a)(a)
(?(2)a)(a)
(?(0)a)
(?(00)a)
(?(01)a)(a)
(?(1a)a)
(?(a)a)
(?P<a>x)(?(a)y|z)
(?P<a>x)(?(b)y|z)
(?()a)
(?(
(?(1
(?(1)
(?(1)a
(?(1)a|
(?(1)a|b
(?(-1)a)
(?(+1)a)
(?(1.0)a)
(?(é)a)
(?(١)a)
(?(1073741823)a)
(?(1073741822)a)
(?(99999999999999999999)a)
(a)(?(1)(?i)b)
(?<=a+)
(?<=a*)
(?<=a?)
(?<=a{3})
(?<=a{2,3})
(?<=a|bc)
(?<=ab|cd)
(?<=(a)|b)
(?<=(ab)|c\d)
(?<=\b)
(?<=^a)
(?<=a$)
(?<=(?=b)a)
(?<=(?<=b)a)
(?<=(?<=b+)a)
(?<=(?:a|bc){2})
(?<=x{4294967294}y)
(?<=x{4294967294}yy)
(?<=x{4294967294}xx)
(?<=(?:x{65536}){65536})
(?<=(?:x{65536}){65537})
(?<=(a)\1)
(a)(?<=\1)
(a+)(?<=\1)
(a)(?<=(?P=a))
(?P<a>a)(?<=(?P=a))
(?<=(a)(?P=a))
(?<=(?P<a>a)(?P=a))
(a)(?<=(?(1)b|c))
(a)(?<=(?(1)b|cd))
(?<=(?(1)b|c))(a)
(?<=(a)(?(1)b|c))
(a)(?<=(?(2)b|c))(c)
(?<=a)(?<=b+)(?<=c)
(?<=a+)(?<=b{1000000000000})
(?<=(?:)*)
(?<=(?:a{0})*)
(?<=a{0,1}?)
(?<=a(?#x){2})
(?<=a\Z)
(?<=a(?!b+))
(?<=a(?=b+))
(?<=[ab])
(?<=.)
(?<=\d\w\s)
(?<=\x41\u0041)
(?<=(?i)a)
(?<=(?i:a))
(?<=(?>a))
(?<=(?>a+))
(?<=a++)
(?<=a{2}+)
\N{EM DASH}
\N{EM DASH
\N{
\N{}
\N
\N{ EM DASH}
\N{EM  DASH}
\N{LATIN CAPITAL LETTER GHA}
\N{LATIN SMALL LETTER R WITH TILDE}
\N{CJK UNIFIED IDEOGRAPH-4E00}
\N{HANGUL SYLLABLE GA}
\N{em-dash}
\N{EM_DASH}
\N{EM DASH!}
\x
\x4
\x41
\x4G
\xG4
\u
\u004
\u0041
\u00e9
\ud800
\udfff
\ud800\udc00
[\ud800-\udfff]
[\ud7ff-\ue000]
\U
\U0000004
\U00000041
\U0001F600
\U0010FFFF
\U00110000
\UFFFFFFFF
\U0000D800
\/
\-
\.
\
\é
\€
\_
\#
\'
\"
\<
\>
\=
\@
\!
\%
\&
\~
\`
\:
\;
\,
(?<name>x)
(?<name>x)\k<name>
\p{L}+
\p{Lu}
[\p{L}]
\/api\/v1
[^]
\cA
[\cA]
\u{41}
\x{41}
[\x{41}]
(?i)\u{41}
^\/(?:[^\/]+\/)*$
^[\w-]+$
^[\w-.]+$
^[\w.-]+$
^[a-zA-Z0-9-_]+$
^[a-zA-Z0-9_-]+$
^[A-Za-z0-9.\-_]+$
^[\-\w]+$
^\S+@\S+\.\S+$
[a-z]{2,}(?<!-)$
^(?!-)[a-z0-9-]{1,63}(?<!-)$
\d{4}-\d{2}-\d{2}
^\d{3}(?:\d{2})?$
((((((((((a))))))))))
(?:(?:(?:(?:(?:(?:(?:(?:(?:(?:a))))))))))
a{1}{2}
a{1}?{2}
a{1}??
a{,1}?
a{,}?
a{,}+
a{}?
a{}*
a{,
a{1,
a{1
a{1}?+
.*+
.*?+
(*)
(+)
(?)
(|)
(a|)
(|a)
[|]
[(]
[)]
[{]
[}]
[*]
[+]
[?]
[.]
[$]
[^^]
[\^]
[a^]
\(
\)
\[
\]
\{
\}
\*
\+
\?
\|
\^
\$
\\\\
\\
""".split("\n")[1:-1]

# Extras that are awkward to write in the raw block above.
HAND += [
    "",
    " ",
    "\n",
    "a\nb",
    "(?x)a\n# c\nb",
    "(?x)[a\n]",
    "a\n(",
    "a\nb\n[",
    "\t",
    "\\\n",
    "[\\\n]",
    "a\\\nb",
    "é",
    "(?P<a\u00e9>x)",
    "(?P<\U0001d518>x)",
    "(?P<a\u200db>x)",
    "(?P<a\u0301>x)",
    "(?P<\u0301a>x)",
    "(?P<a b\u3000>x)",
    "(?P<\x00>x)",
    "(?P<\x7f>x)",
    "(?P<\u00a0>x)",
    "(?P<\u2028>x)",
    "(?P<\U000e0001>x)",
    "(?P<'>x)",
    "(?P<\">x)",
    "(?P<'\">x)",
    "(?P<\\>x)",
    "(?P<\\n>x)",
    "(?P<a\\>b>x)",
    "(?P<a\tb>x)",
    "(?P=\u00e9)",
    "(?(\u00e9\u0301)a)",
    "(?(a\\)b)c)",
    "(?i\u00e9)",
    "(?i\u0661)",
    "(?i\u2160)",
    "(?i-\u00e9:x)",
    "(?-\u00e9:x)",
    "(?\u00e9)",
    "(?P\u00e9)",
    "(?<\u00e9)",
    "\\N{" + "A" * 300 + "}",
    "a{" + "9" * 4301 + "}",
    "a{" + "9" * 4300 + "}",
    "a{1," + "9" * 4301 + "}",
    "(?(" + "1" * 4301 + ")a)",
    "(?(" + "1" * 4300 + ")a)",
    "(?(" + "0" * 50 + "1)a)(b)",
    "\U0001F600+",
    "[\U0001F600-\U0001F64F]",
    "[\U0001F64F-\U0001F600]",
    "(?<=\U0001F600)a",
    "\u212a",
    "[\u0130]",
]


# Deeply nested patterns (RecursionError boundary at this call depth).
def nested(n, open_="(", close=")", inner="a"):
    return open_ * n + inner + close * n


DEEP = []
for n in (60, 100, 200, 480, 484, 485, 486, 487, 490, 495, 496, 600, 1000):
    DEEP.append(nested(n))
    DEEP.append(nested(n, "(?:"))
    DEEP.append(nested(n, "(?="))
    DEEP.append(nested(n, "(?<=", ")", "a"))
    DEEP.append(nested(n, "(?>"))
    DEEP.append(nested(n, "(?i:"))
    DEEP.append(nested(n, "(?P<g", ")", "a"))  # bad: names repeat -> fine either way
    DEEP.append(nested(n, "[a](", ")"))
    DEEP.append("(a)" + nested(n, "(?(1)", ")"))
    DEEP.append("(a)" + nested(n, "(?(1)b|", ")"))
for n in (960, 965, 968, 969, 970, 971, 972, 973, 975, 980, 989, 990, 1000):
    DEEP.append("(a)" + nested(n, "(?(1)", ")"))
DEEP.append("(" * 5000)
DEEP.append(")" * 5000)
DEEP.append("a" * 100000)
DEEP.append("(a)" * 2000)
DEEP.append("|".join(["a"] * 5000))


# Targeted matching semantics: (pattern, texts).
MATCH_HAND = [
    (r"a$", ["a", "a\n", "a\n\n", "a\nb", "\na"]),
    (r"a$\n", ["a\n", "a"]),
    (r"(?m)a$", ["a\nb", "a\r\n", "ab"]),
    (r"(?m)^b", ["a\nb", "a\rb", "b"]),
    (r"(?m)^$", ["", "\n", "a\n", "a"]),
    (r"^$", ["", "\n", "\n\n", "a"]),
    (r"$", ["", "\n", "a"]),
    (r"a\Z", ["a", "a\n"]),
    (r"\Aa", ["a", "ba"]),
    (r"\B", ["", "a", " ", "ab"]),
    (r"\b", ["", "a", " ", "-"]),
    (r"x\b", ["x\u00e9", "x\u00b2", "x\u0301", "x\u203f", "x\u24b6", "x_", "x\u0663", "x\u2160"]),
    (r"(?a)x\b", ["x\u00e9", "xa", "x_", "x\u0663"]),
    (r"(?a)\w", ["\u00e9", "_", "\u0663"]),
    (r"(?a)\d", ["\u0663", "5"]),
    (r"(?a)\s", ["\x1c", "\x85", "\xa0", "\x0b", " "]),
    (r"\s", ["\x1c", "\x1f", "\x85", "\u200b", "\u180e", "\ufeff"]),
    (r"\d", ["\u0663", "\u00b2", "\u2160", "\U0001d7ce"]),
    (r"\w", ["\u00b2", "\u2160", "\u0301", "\u203f", "\u24b6", "\u00aa", "\u02b0"]),
    (r".", ["\n", "\r", "\u2028", ""]),
    (r"(?s).", ["\n"]),
    (r"(?i)k", ["K", "\u212a", "k"]),
    (r"(?ai)k", ["K", "\u212a"]),
    (r"(?i)[k-l]", ["\u212a", "L"]),
    (r"(?ai)[k-l]", ["\u212a", "L"]),
    (r"(?ai)[^k]", ["\u212a", "K", "k"]),
    (r"(?i)i", ["I", "\u0130", "\u0131", "i"]),
    (r"(?i)\u0130", ["i", "I", "\u0131"]),
    (r"(?i)[h-j]", ["\u0130", "\u0131"]),
    (r"(?i)[^i]", ["\u0130", "\u0131", "I", "x"]),
    (r"(?i)\u00df", ["\u1e9e", "ss", "SS"]),
    (r"(?i)\u03c3", ["\u03a3", "\u03c2"]),
    (r"(?i)(a)\1", ["aA", "Aa"]),
    (r"(?ai)(k)\1", ["kK", "k\u212a"]),
    (r"(?i)(\u0130)\1", ["\u0130i", "\u0130I"]),
    (r"(a)|\1b", ["b", "ab"]),
    (r"(a)?\1", ["", "b", "aa"]),
    (r"(a)?(?(1)b|c)", ["c", "ab", "b", "ac"]),
    (r"(?(2)a|b)(c)?", ["b", "bc", "ac", "a"]),
    (r"(?P<x>a)?(?(x)b|c)", ["c", "ab"]),
    (r"(?:(a)|b)+(?(1)x|y)", ["ax", "bx", "by", "bax", "aby", "abx"]),
    (r"(?>a+)a", ["aaa", "a"]),
    (r"a++a", ["aaa"]),
    (r"a*+b", ["aaab", "b"]),
    (r"(?:ab)?+b", ["ab", "abb"]),
    (r"a{2,3}+a", ["aaa", "aaaa"]),
    (r"(?<=a)b", ["ab", "b"]),
    (r"(?<!a)b", ["ab", "b", "cb"]),
    (r"(?<=ab|cd)e", ["abe", "cde", "be"]),
    (r"(?<=\b)a", ["a", "ba", " a"]),
    (r"(?<=^)a", ["a", "ba"]),
    (r"(?<=a$)", ["a", "a\n", "ab"]),
    (r"(?=a$)", ["a", "a\n", "ab"]),
    (r"(?!a$)a", ["a", "a\n", "ab"]),
    (r"(?:a|\b)*b", ["b", "ab", " b"]),
    (r"(?:\b)+a", ["a", " a"]),
    (r"(?:(?=a))*a", ["a"]),
    (r"((?=a))*a", ["a"]),
    (r"((?=a))+a", ["a", "b"]),
    (r"(?:(a)(?=b))*(?(1)b|c)", ["ab", "c", "b"]),
    (r"(?:)*", ["", "a"]),
    (r"(?!)", ["", "a"]),
    (r"(?<!)", ["", "a"]),
    (r"(?=)", ["", "a"]),
    (r"(?<=)", ["", "a"]),
    (r"x(?<=x)", ["x"]),
    (r"[\ud800-\udfff]", ["", "a", "\ufffd"]),
    (r"[^\ud800]", ["a", "\n"]),
    (r"\ud800|a", ["a", "b"]),
    (r"[\ud7ff-\ue000]", ["\ud7ff", "\ue000", "a"]),
    (r"\x{41}", ["x{41}", "A"]),
    (r"a{,2}b", ["b", "aab", "a{,2}b"]),
    (r"a{,}b", ["b", "aaab"]),
    (r"a{1,2", ["a{1,2", "aa"]),
    (r"a{}", ["a{}", "a"]),
    (r"{", ["{"]),
    (r"x{2}{", ["xx{"]),
    (r"(?x) a b # c", ["ab", "a b"]),
    (r"(?x)a\ b", ["a b", "ab"]),
    (r"(?x)[ ]", [" ", "a"]),
    (r"(?x: a )b", ["ab"]),
    (r"(?-x: a )", [" a ", "a"]),
    (r"(?i:a)A", ["aA", "AA", "Aa"]),
    (r"(?i)(?-i:a)A", ["aA", "AA", "aa"]),
    (r"(?s:.)\.", ["\n.", "a."]),
    (r"(?m:^a)", ["b\na"]),
    (r"(?a:\w)\w", ["\u00e9\u00e9", "a\u00e9"]),
    (r"(?a)(?u:\w)", ["\u00e9"]),
    (r"(?a:\W)", ["\u00e9", "-"]),
    (r"(?a)(?u:\w|\d)", ["\u00e9", "a"]),
    (r"(?a)(?u:\wa|\wb)", ["\u00e9a", "xa"]),
    (r"(?a)((?u:\w))", ["\u00e9"]),
    (r"(?a)(?:)(?u:\w)", ["\u00e9"]),
    (r"(?a)(?u:)(?u:\w)", ["\u00e9"]),
    (r"(?a)(?u:[\w-])", ["\u00e9", "-"]),
    (r"(?a)(?u:[^\W])", ["\u00e9"]),
    (r"(?a)(?iu:[\wk])", ["\u00e9"]),
    (r"(?a)(?iu:[\w0])", ["\u00e9"]),
    (r"(?a)(?u:\w)?", ["\u00e9"]),
    (r"(?a)(?u:\w)x", ["\u00e9x"]),
    (r"(?a)x(?u:\w)", ["x\u00e9"]),
    (r"(?a)(?u:\s)", ["\u2003", " "]),
    (r"(?a:\D)", ["\u0663", "a"]),
    (r"(?a)(?u:\w*)\u00e9", ["\u00e9"]),
    (r"(?a)(?u:\w|a)", ["\u00e9"]),
    (r"(?a)(?u:\w|ab)", ["\u00e9"]),
    (r"\0", ["\x00"]),
    (r"\012", ["\n"]),
    (r"\x0a", ["\n"]),
    (r"[\b]", ["\x08", "b"]),
    (r"\N{EM DASH}", []),
    (r"[\w-]", ["-", "_", "a"]),
    (r"[\d-z]", ["-", "5", "z", "y"]),
    (r"[a-]", ["-"]),
    (r"[]a]", ["]", "a"]),
    (r"[^]a]", ["]", "b"]),
    (r"\s+$", [" \n", "a "]),
    (r"^[\w.-]+@[\w-]+\.[a-z]{2,}$", ["user.name@example.com", "us\u00e9r@exa_mple.com\n", "a@b.c"]),
    (r"^\w{1,64}$", ["a" * 64, "a" * 65, "\u00e9" * 10]),
    (r"^.{1,255}$", ["x" * 255, "x" * 256, "x" * 255 + "\n"]),
    (r"^(a+)+$", ["a" * 20 + "!"]),
]
MATCH_HAND = [(p, [t.encode("ascii", "backslashreplace").decode("unicode_escape") for t in ts]) for p, ts in MATCH_HAND]

# --------------------------------------------------------------------------
# ECMAScript-isms and other foreign syntax
# --------------------------------------------------------------------------
ECMA = [
    r"(?<year>\d{4})-(?<month>\d{2})",
    r"\k<year>",
    r"(?<a>x)\k<a>",
    r"\p{L}",
    r"\p{Letter}",
    r"\P{L}",
    r"\p{Script=Latin}",
    r"[\p{L}\p{N}]",
    r"\u{1F600}",
    r"\/",
    r"^\/users\/\d+$",
    r"[^]",
    r"[^]a]",
    r"\cA",
    r"\cz",
    r"[\cA-\cZ]",
    r"\0",
    r"\v",
    r"[\b]",
    r"(?:)",
    r"a{,5}",
    r"\x{41}",
    r"[[:alpha:]]",
    r"[[:digit:]]+",
    r"[\w-\.]",
    r"[\w-]",
    r"[.-\w]",
    r"\d+(?=%)",
    r"(?<=\$)\d+",
    r"(?<=\$|USD)\d+",
    r"(?<!\w)",
    r"(?=.*[A-Z])(?=.*\d).{8,}",
    r"^(?=.*[a-z])(?=.*[A-Z])(?=.*\d)(?=.*[@$!%*?&])[A-Za-z\d@$!%*?&]{8,}$",
    r"\h",
    r"\R",
    r"\X",
    r"\K",
    r"\G",
    r"\Q.\E",
    r"(?|a|b)",
    r"(?'name'x)",
    r"(?P<name>x)(?P=name)",
    r"(?&name)",
    r"(?R)",
    r"(?1)",
    r"a++",
    r"(?>a)",
    r"\z",
    r"\Z",
    r"$",
    r"(?u)\w",
    r"(?s:.)",
    r"(?-i:a)",
    r"[a-z&&[^aeiou]]",
    r"\e",
    r"\a",
    r"\f",
    r"\t",
    r"\n",
    r"\r",
    r"\8",
    r"\9",
    r"\1",
    r"(a)\1",
    r"\B",
    r"\b",
    r"[\s\S]",
    r"[\d\D]",
    r"[\w\W]",
    r".",
    r"\.",
    r"^$",
    r"\u0000",
    r"\x00",
    r"[\x00-\x1F]",
    r"[\u0000-\u001F\u007F]",
    r"[\u4e00-\u9fa5]",
    r"[\uD800-\uDBFF][\uDC00-\uDFFF]",
    r"\uD83D\uDE00",
    r"[\u{1F600}-\u{1F64F}]",
    r"(?i)abc",
    r"(?i:abc)",
    r"(?m)^abc$",
    r"(?s)a.b",
    r"(?x)a b",
    r"(?n)a",
    r"(?U)a",
    r"(?J)a",
    r"(?^)a",
    r"(?d)a",
    r"(?g)a",
    r"/abc/i",
    r"/abc/",
    r"\p{Lu}\p{Ll}+",
    r"\p{IsLatin}",
    r"\p{InBasicLatin}",
    r"\w+@\w+\.\w+",
    r"\x",
    r"\xG",
    r"\u",
    r"\u12",
    r"\c",
    r"\c1",
    r"\k",
    r"\k<>",
    r"\k'x'",
    r"\g{1}",
    r"\g-1",
    r"{",
    r"}",
    r"]",
    r"x{2,}?",
    r"x{2}?",
    r"x{,2}",
    r"a{2,3}{4}",
]


# --------------------------------------------------------------------------
# Real-world patterns
# --------------------------------------------------------------------------
def spec_patterns(paths):
    pats = []
    seen = set()

    def walk(o):
        if isinstance(o, dict):
            for k, v in o.items():
                if k == "pattern" and isinstance(v, str) and v not in seen:
                    seen.add(v)
                    pats.append(v)
                walk(v)
        elif isinstance(o, list):
            for v in o:
                walk(v)

    for p in paths:
        with open(p) as f:
            walk(json.load(f))
    return pats


COMMON = [
    r"^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$",
    r"^[^@\s]+@[^@\s]+\.[^@\s]+$",
    r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$",
    r"^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$",
    r"^\d{4}-\d{2}-\d{2}$",
    r"^\d{4}-(0[1-9]|1[0-2])-(0[1-9]|[12]\d|3[01])$",
    r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})$",
    r"^([01]\d|2[0-3]):[0-5]\d$",
    r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\.(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\+([0-9a-zA-Z-]+(?:\.[0-9a-zA-Z-]+)*))?$",
    r"^v?\d+\.\d+\.\d+$",
    r"^https?://[^\s/$.?#].[^\s]*$",
    r"^(https?|ftp)://[^\s/$.?#].[^\s]*$",
    r"^((25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)$",
    r"^(?:[0-9]{1,3}\.){3}[0-9]{1,3}$",
    r"^(([0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4})$",
    r"^\+?[1-9]\d{1,14}$",
    r"^\+[1-9]\d{1,14}$",
    r"^\(?\d{3}\)?[-.\s]?\d{3}[-.\s]?\d{4}$",
    r"^#?([a-fA-F0-9]{6}|[a-fA-F0-9]{3})$",
    r"^#[0-9A-Fa-f]{6}$",
    r"^[a-z0-9]+(?:-[a-z0-9]+)*$",
    r"^[a-z][a-z0-9_]*$",
    r"^[A-Z]{2}$",
    r"^[A-Z]{3}$",
    r"^[a-z]{2}(-[A-Z]{2})?$",
    r"^[A-Za-z0-9-_]+\.[A-Za-z0-9-_]+\.[A-Za-z0-9-_]*$",
    r"^[A-Za-z0-9+/]+={0,2}$",
    r"^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$",
    r"^[\w\-\.]+$",
    r"^[\w.-]+$",
    r"^[\w ]+$",
    r"^\S+$",
    r"^\s*$",
    r"^.{1,255}$",
    r"^.{0,1024}$",
    r"^[^\s]+$",
    r"^[^<>]*$",
    r"^[^\\/:*?\"<>|]+$",
    r"^\d+$",
    r"^-?\d+(\.\d+)?$",
    r"^[0-9]+(\.[0-9]{1,2})?$",
    r"^\d{5}(-\d{4})?$",
    r"^[A-Z]{1,2}\d[A-Z\d]? ?\d[A-Z]{2}$",
    r"^[0-9]{3}-[0-9]{2}-[0-9]{4}$",
    r"^arn:aws:[a-z0-9-]+:[a-z0-9-]*:\d{12}:.+$",
    r"^projects/[^/]+/locations/[^/]+$",
    r"^/.*",
    r"^(GET|POST|PUT|DELETE|PATCH)$",
    r"^(?i)(true|false)$",
    r"(?i)^(true|false)$",
    r"^(?i:true|false)$",
    r"^[a-zA-Z_][a-zA-Z0-9_]*$",
    r"^[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)*$",
    r"^(\*|[0-9]+(\.[0-9]+)*)$",
    r"^\$[A-Z_][A-Z0-9_]*$",
    r"^x-[\w\d\.\-\_]+$",
    r"^x-",
    r"^/",
    r"^[1-5]\d\d$",
    r"^[1-5](?:\d{2}|XX)$",
    r"^\d{3}$",
    r"^[1-5]XX$",
    r"^default$",
    r"^[a-zA-Z0-9\.\-_]+$",
    r"^(\.\.?/)?[\w./-]+$",
    r"\.(jpg|jpeg|png|gif)$",
    r"^data:image/(png|jpeg);base64,[A-Za-z0-9+/=]+$",
    r"^\w+\s\w+$",
    r"^[\p{L} ]+$",
    r"^[À-ÿa-zA-Z ]+$",
    r"^[ぁ-んァ-ン一-龥]+$",
    r"^[\u4e00-\u9fff]+$",
    r"^[\u0600-\u06FF\s]+$",
    r"^\d{1,3}(,\d{3})*(\.\d+)?$",
    r"^(?!.*\.\.)(?!\.)[\w.]{1,30}(?<!\.)$",
    r"^(?=.{8,}$)(?=.*[A-Z])(?=.*[a-z])(?=.*\d).*$",
    r"^(?:(?:\+|00)33|0)\s*[1-9](?:[\s.-]*\d{2}){4}$",
    r"^[A-Z]{2}\d{2}[A-Z0-9]{1,30}$",
    r"^[0-9]{13,19}$",
    r"^4[0-9]{12}(?:[0-9]{3})?$",
    r"^(?:4[0-9]{12}(?:[0-9]{3})?|5[1-5][0-9]{14})$",
    r"^\d+(ms|s|m|h)$",
    r"^P(?!$)(\d+Y)?(\d+M)?(\d+W)?(\d+D)?(T(?=\d)(\d+H)?(\d+M)?(\d+S)?)?$",
    r"^([a-z0-9]+\.)*[a-z0-9]+$",
    r"^\*$|^\d+$",
    r"^\b\w+\b$",
    r"\bword\b",
    r"\Bend",
    r"^(?P<major>\d+)\.(?P<minor>\d+)$",
    r"^(\w+)\s\1$",
    r"^(?P<q>['\"]).*(?P=q)$",
    r"^[^\x00-\x1F\x7F]*$",
    r"^[\x20-\x7E]*$",
    r"^[ -~]+$",
    r"^\p{L}+$",
    r"(?m)^#",
    r"(?s)^.*$",
    r"^a.c$",
    r"[[:alpha:]]+",
    r"^$",
    r"",
    r".*",
    r"^.*$",
    r"^[^/]+$",
    r"^[a-zA-Z]+(?:[ '-][a-zA-Z]+)*$",
    r"^\s+|\s+$",
    r"^(\d+)(?:\.(\d+))?(?:\.(\d+))?$",
    r"^[a-f0-9]{40}$",
    r"^[A-Fa-f0-9]{64}$",
    r"^id_(test|live)_[A-Za-z0-9]{24,}$",
    r"^ghp_[A-Za-z0-9]{36}$",
    r"^gh[pousr]_[A-Za-z0-9_]{36,251}$",
    r"^(?:\d{1,3}\.){3}\d{1,3}/\d{1,2}$",
    r"^[a-z]+://",
    r"^\w+$",
    r"\w+",
    r"^\d+\s*(?:KB|MB|GB)$",
    r"(?i)^[a-z]+$",
    r"(?i)^[A-Z0-9]+$",
    r"(?i)kelvin",
    r"(?i)straße",
    r"(?i)i",
    r"(?a)\w+",
    r"(?a)^\d+$",
    r"(?a:\s)",
    r"^\d+$(?#trailing)",
    r"(?x)^ \d{3} - \d{4} $",
    r"(?x) ^ [a-z]+ # letters\n $",
]


# --------------------------------------------------------------------------
# Random fuzz
# --------------------------------------------------------------------------
FUZZ_TOKENS = (
    list("aaabbbcxyz019") * 3
    + list("()[]{}*+?|^$.,-:<>=!#") * 2
    + [
        "\\",
        "(?",
        "(?:",
        "(?P<",
        "(?P<a>",
        "(?P<b>",
        "(?P=a)",
        "(?P=b)",
        "(?P",
        "(?#",
        "(?=",
        "(?!",
        "(?<=",
        "(?<!",
        "(?<",
        "(?>",
        "(?(",
        "(?(1)",
        "(?(a)",
        "(?i)",
        "(?x)",
        "(?a)",
        "(?u)",
        "(?L)",
        "(?i:",
        "(?-i:",
        "(?s-m:",
        "(?ai:",
        "\\d",
        "\\D",
        "\\w",
        "\\W",
        "\\s",
        "\\S",
        "\\b",
        "\\B",
        "\\A",
        "\\Z",
        "\\z",
        "\\1",
        "\\2",
        "\\10",
        "\\0",
        "\\01",
        "\\177",
        "\\400",
        "\\8",
        "\\x4",
        "\\x41",
        "\\u00e9",
        "\\u12",
        "\\U0001F600",
        "\\U00110000",
        "\\N{EM DASH}",
        "\\N{",
        "\\N",
        "\\n",
        "\\t",
        "\\-",
        "\\]",
        "\\[",
        "\\(",
        "\\)",
        "\\.",
        "\\/",
        "\\k",
        "\\p",
        "\\c",
        "\\e",
        "{1}",
        "{2,3}",
        "{,2}",
        "{3,}",
        "{3,1}",
        "{}",
        "{,}",
        "*?",
        "+?",
        "??",
        "*+",
        "++",
        "?+",
        "[^",
        "[a-z]",
        "[^\\d]",
        "[\\w-]",
        "-]",
        "é",
        "ß",
        "İ",
        " ",
        "\n",
        "\t",
        "_",
        "&",
        "~",
    ]
)


def fuzz_pattern():
    n = rng.choice([1, 2, 2, 3, 3, 4, 4, 5, 6, 7, 8, 10, 12, 16])
    return "".join(rng.choice(FUZZ_TOKENS) for _ in range(n))


def balanced_fuzz():
    """Structured fuzz: more likely to be valid, to exercise deeper semantics."""

    def atom(d):
        r = rng.random()
        if d > 3 or r < 0.35:
            return rng.choice(
                [
                    "a", "b", "c", "x", "0", "1", ".", "\\d", "\\w", "\\s", "\\W", "\\S", "\\D",
                    "[ab]", "[^a]", "[a-c]", "[\\d_]", "[^\\W\\d]", "\\b", "\\B", "^", "$",
                    "\\A", "\\Z", "é", "\\.", "\\n", "\\x41", "[İı]", "I", "i", "K", "k",
                    "\u212a", "ß", "-", " ", "\\-", "[.-]",
                ]
            )
        if r < 0.55:
            return atom(d + 1) + rng.choice(["*", "+", "?", "{2}", "{1,3}", "{,2}", "*?", "+?", "??", "*+", "++", "{2,}?", "{0}"])
        if r < 0.75:
            return "(" + rng.choice(["", "?:", "?P<n%d>" % rng.randrange(3), "?>", "?i:", "?-i:", "?s:", "?m:", "?a:", "?x:"]) + seq(d + 1) + ")"
        if r < 0.85:
            return "(" + rng.choice(["?=", "?!", "?<=", "?<!"]) + seq(d + 1) + ")"
        if r < 0.92:
            return seq(d + 1) + "|" + seq(d + 1)
        if r < 0.96:
            return rng.choice(["\\1", "\\2", "(?P=n0)", "(?P=n1)"])
        return "(?(" + rng.choice(["1", "2", "n0"]) + ")" + seq(d + 1) + rng.choice(["", "|" + seq(d + 1)]) + ")"

    def seq(d):
        return "".join(atom(d) for _ in range(rng.randrange(1, 4)))

    prefix = rng.choice(["", "", "", "", "(?i)", "(?m)", "(?s)", "(?a)", "(?x)", "(?ims)", "(?ai)"])
    return prefix + seq(0)


# --------------------------------------------------------------------------
# Text generation for matching tests (walks the sre parse tree)
# --------------------------------------------------------------------------
CAT_SAMPLES = {
    C.CATEGORY_DIGIT: list("0123456789") + ["\u0663", "\uff11", "\U0001d7ce"],
    C.CATEGORY_NOT_DIGIT: list("aZ_ -.\n") + ["\u00e9", "\u00b2"],
    C.CATEGORY_SPACE: [" ", "\t", "\n", "\r", "\x0b", "\x0c", "\x1c", "\x1f", "\x85", "\xa0", "\u2003", "\u3000"],
    C.CATEGORY_NOT_SPACE: list("a0_-.") + ["\u00e9", "\u200b"],
    C.CATEGORY_WORD: list("azAZ09_") + ["\u00e9", "\u00df", "\u0130", "\u00b2", "\u2160", "\u4e00", "\u0663"],
    C.CATEGORY_NOT_WORD: list(" -.!@\n") + ["\u0301", "\u203f", "\u24b6", "\u00a0"],
}
NOISE = list("abcxyzABCXYZ0123456789_-. /@:\n\t") + [
    "\u00e9", "\u00df", "\u0130", "\u0131", "\u212a", "\u017f", "\u0663", "\u00b2",
    "\u0301", "\u2003", "\x1c", "\U0001F600", "\u4e00", "\u00a0",
]


class Gen:
    def __init__(self):
        self.groups = {}

    def seq(self, sp, depth=0):
        out = []
        for op, av in sp:
            out.append(self.node(op, av, depth))
        return "".join(out)

    def node(self, op, av, depth):
        if depth > 30:
            return ""
        if op is C.LITERAL:
            return safe_chr(av)
        if op is C.NOT_LITERAL:
            return rng.choice([c for c in "ab0 _-" if ord(c) != av] or ["z"])
        if op is C.ANY:
            return rng.choice(NOISE[:-1])
        if op is C.IN:
            return self.pick_in(av)
        if op is C.BRANCH:
            return self.seq(rng.choice(av[1]), depth + 1)
        if op is C.SUBPATTERN:
            g, _, _, p = av
            s = self.seq(p, depth + 1)
            if g:
                self.groups[g] = s
            return s
        if op is C.ATOMIC_GROUP:
            return self.seq(av, depth + 1)
        if op in (C.MAX_REPEAT, C.MIN_REPEAT, C.POSSESSIVE_REPEAT):
            lo, hi, p = av
            hi = min(hi, lo + 3)
            n = rng.randint(lo, hi) if lo <= 50 else 0
            return "".join(self.seq(p, depth + 1) for _ in range(n))
        if op is C.GROUPREF:
            return self.groups.get(av, "")
        if op is C.GROUPREF_EXISTS:
            g, yes, no = av
            if g in self.groups:
                return self.seq(yes, depth + 1)
            return self.seq(no, depth + 1) if no else ""
        if op is C.AT:
            if av in (C.AT_END, C.AT_END_LINE) and rng.random() < 0.2:
                return "\n"
            return ""
        return ""

    def pick_in(self, items):
        neg = items and items[0][0] is C.NEGATE
        if neg:
            for _ in range(10):
                c = rng.choice(NOISE)
                if re.fullmatch(_in_to_regex(items), c):
                    return c
            return "\u2603"
        op, av = rng.choice(items)
        if op is C.LITERAL:
            return safe_chr(av)
        if op is C.RANGE:
            lo, hi = av
            return safe_chr(rng.randint(lo, min(hi, lo + 200)))
        if op is C.CATEGORY:
            return rng.choice(CAT_SAMPLES.get(av, ["a"]))
        return "a"


def _in_to_regex(items):
    return "[^" + "".join(_item_src(op, av) for op, av in items[1:]) + "]"


def _item_src(op, av):
    if op is C.LITERAL:
        return re.escape(safe_chr(av))
    if op is C.RANGE:
        return re.escape(safe_chr(av[0])) + "-" + re.escape(safe_chr(av[1]))
    if op is C.CATEGORY:
        return {
            C.CATEGORY_DIGIT: r"\d", C.CATEGORY_NOT_DIGIT: r"\D", C.CATEGORY_SPACE: r"\s",
            C.CATEGORY_NOT_SPACE: r"\S", C.CATEGORY_WORD: r"\w", C.CATEGORY_NOT_WORD: r"\W",
        }[av]
    return ""


def safe_chr(cp):
    if 0xD800 <= cp <= 0xDFFF:
        return "\ufffd"
    return chr(cp)


def mutate(s):
    s = list(s)
    for _ in range(rng.randint(1, 2)):
        r = rng.random()
        if r < 0.3 and s:
            del s[rng.randrange(len(s))]
        elif r < 0.6:
            s.insert(rng.randint(0, len(s)), rng.choice(NOISE))
        elif s:
            s[rng.randrange(len(s))] = rng.choice(NOISE)
        else:
            s.append(rng.choice(NOISE))
    return "".join(s)


def texts_for(pattern, k=6):
    out = set()
    try:
        tree = _parser.parse(pattern)
    except Exception:
        return []
    for _ in range(k * 3):
        if len(out) >= k:
            break
        g = Gen()
        try:
            s = g.seq(tree)
        except Exception:
            s = ""
        if len(s) > 60:
            s = s[:60]
        r = rng.random()
        if r < 0.45:
            out.add(s)
        elif r < 0.8:
            out.add(mutate(s))
        elif r < 0.9:
            out.add(s + "\n")
        else:
            out.add("".join(rng.choice(NOISE) for _ in range(rng.randint(0, 6))))
    out.add("")
    return sorted(out)


SPEC_SAMPLES = [
    "github", "GitHub", "sha256:" + "a" * 64, "sha256:" + "A" * 64, "/v1/charges", "/v1/charges/ch_1",
    "/v1/financial_connections/accounts/fca_1", "/v1/tax/calculations/tc_1/line_items", "abc_token_12",
    "prefix_" + "a" * 32, "updated_secret_" + "A1" * 8, "https://example.com", "http://x", "refs/heads/main",
    "refs/tags/v1", "pkg:npm/x", "1.2.3", "1.2", "ssh-rsa AAAA", "ecdsa-sha2-nistp256 AAAA", "QUJD", "QUI=",
    "QQ==", "Q", "user@example.com", "a.b-c_d", "a b", "", "\n", "abc\n", "123", "-1", "x-foo", "issue",
    "0" * 40, "f" * 64, "2024-01-31", "2024-13-01", "12:30", "+14155552671", "#fff", "#GGG", "e5d0f1c2-1a2b-4c3d-8e9f-0a1b2c3d4e5f",
]


# --------------------------------------------------------------------------
# Character-class tables and case folding
# --------------------------------------------------------------------------
def ranges_of(pred):
    out = []
    start = None
    for cp in range(0x110000 + 1):
        ok = cp <= 0x10FFFF and not (0xD800 <= cp <= 0xDFFF) and pred(chr(cp))
        if ok and start is None:
            start = cp
        elif not ok and start is not None:
            out.append([start, cp - 1])
            start = None
    return out


CLASS_PATTERNS = [
    r"\w", r"\W", r"\d", r"\D", r"\s", r"\S", r".", r"(?s).", r"[^\W\d]", r"[\w\s]", r"[^a-z\d]",
    r"(?a)\w", r"(?a)\W", r"(?a)\d", r"(?a)\s", r"(?a)\S", r"(?a)[^\W\d]",
    r"\b.", r".\b", r"(?a)\b.", r"\B.", r"(?i)k", r"(?i)[a-z]", r"(?i)[^a-z]", r"(?ai)[a-z]",
    r"(?ai)k", r"(?i)\w", r"(?i)[\d\s]", r"(?i)[a\W]", r"(?i)[^k\d]", r"(?i)σ", r"(?i)[ǅ]", r"(?i)i",
    r"(?i)[i-k]", r"(?i)ß", r"(?i)\u0345", r"(?i)[\u0345]", r"(?i)θ", r"(?ai)[^k]", r"[\u0300-\u036f]",
]


def casefold_cases():
    """Single-character case-insensitive matching for every cased character."""
    lower_groups = {}
    for cp in range(0x110000):
        if 0xD800 <= cp <= 0xDFFF:
            continue
        ch = chr(cp)
        lo = ch.lower()
        if len(lo) == 1:
            lower_groups.setdefault(lo, set()).add(ch)
        up = ch.upper()
        if len(up) == 1:
            lower_groups.setdefault(up.lower() if len(up.lower()) == 1 else up, set()).add(ch)
    cases = []
    for cp in range(0x110000):
        if 0xD800 <= cp <= 0xDFFF:
            continue
        ch = chr(cp)
        if ch.lower() == ch and ch.upper() == ch and ch.casefold() == ch:
            continue
        cands = set()
        for v in (ch.lower(), ch.upper(), ch.title(), ch.casefold(), ch.swapcase()):
            if len(v) == 1:
                cands.add(v)
                cands |= lower_groups.get(v, set())
                cands |= lower_groups.get(v.lower(), set())
        cands |= lower_groups.get(ch.lower(), set()) if len(ch.lower()) == 1 else set()
        cands.discard(ch)
        for pat_kind in ("lit", "cls"):
            pat = "(?i)" + (re.escape(ch) if pat_kind == "lit" else "[" + re.escape(ch) + "]")
            matches = sorted(c for c in cands if re.fullmatch(pat, c))
            nonmatches = sorted(c for c in cands if not re.fullmatch(pat, c))
            cases.append({"pattern": pat, "match": "".join(matches), "nomatch": "".join(nonmatches)})
    return cases


def main():
    specs = sys.argv[1:]
    real = spec_patterns(specs)

    validity = []
    seen = set()

    def add(p, source):
        if p in seen or not no_surrogates(p):
            return
        seen.add(p)
        r = classify(p)
        r["pattern"] = p
        r["source"] = source
        validity.append(r)

    for p in HAND:
        add(p, "hand")
    for p in ECMA:
        add(p, "ecma")
    for p in real:
        add(p, "spec")
    for p in COMMON:
        add(p, "common")
    for p in DEEP:
        add(p, "deep")
    # Variants of every hand/ecma/common pattern: prefixed, suffixed, wrapped.
    base = [r["pattern"] for r in validity if len(r["pattern"]) < 200]
    for p in base:
        for v in ("(" + p + ")", "x" + p, p + ")", "[" + p, p + "*", "(?x)" + p, "(?<=" + p + ")"):
            add(v, "variant")
    for _ in range(9000 * SCALE):
        add(fuzz_pattern(), "fuzz")
    for _ in range(5000 * SCALE):
        add(balanced_fuzz(), "structured")

    kinds = {}
    for r in validity:
        kinds[r["kind"]] = kinds.get(r["kind"], 0) + 1
    print("validity:", len(validity), kinds, file=sys.stderr)

    # Matching pairs
    matching = []
    mseen = set()

    def addm(p, t, source):
        if (p, t) in mseen or not no_surrogates(t):
            return
        mseen.add((p, t))
        try:
            m = re.search(p, t) is not None
        except Exception:
            return
        matching.append({"pattern": p, "text": t, "match": m, "source": source})

    for p, ts in MATCH_HAND:
        add(p, "match-hand")
        for t in ts:
            addm(p, t, "match-hand")
    valid_ok = [r for r in validity if r["kind"] == "ok" and "\\N" not in r["pattern"] and len(r["pattern"]) < 300]
    for r in valid_ok:
        src = r["source"]
        if src in ("deep",):
            continue
        if src in ("spec", "common"):
            for t in SPEC_SAMPLES:
                addm(r["pattern"], t, src)
            k = 12
        elif src in ("hand", "ecma"):
            k = 8
        else:
            k = 4
        for t in texts_for(r["pattern"], k):
            addm(r["pattern"], t, src)
    mk = {}
    for m in matching:
        mk[m["match"]] = mk.get(m["match"], 0) + 1
    print("matching:", len(matching), mk, file=sys.stderr)

    classes = []
    for p in CLASS_PATTERNS:
        rx = re.compile(p)
        m = re.match(r"\(\?[a-zA-Z]+\)", p)
        prefix, rest = (m.group(0), p[m.end():]) if m else ("", p)
        search = prefix + r"\A(?:" + rest + r")\Z"
        assert re.compile(search)
        if SCALE == 1:
            classes.append({"pattern": p, "search": search, "ranges": ranges_of(lambda c: rx.fullmatch(c) is not None)})
    # Word-boundary context: char followed by '\b' relative to a word char.
    print("classes:", len(classes), file=sys.stderr)

    chars = {
        "ident_start": ranges_of(lambda c: c.isidentifier()),
        "ident_continue": ranges_of(lambda c: ("a" + c).isidentifier()),
        "alpha": ranges_of(lambda c: c.isalpha()),
        "printable": ranges_of(lambda c: c.isprintable()),
    } if SCALE == 1 else {"ident_start": [], "ident_continue": [], "alpha": [], "printable": []}

    casefold = casefold_cases() if SCALE == 1 else []
    print("casefold:", len(casefold), file=sys.stderr)

    os.makedirs(OUT, exist_ok=True)
    for name, data in (("chars", chars), ("validity", validity), ("matching", matching), ("classes", classes), ("casefold", casefold)):
        with open(os.path.join(OUT, name + ".json"), "w") as f:
            if isinstance(data, list):
                # one record per line: compact and diff-friendly
                f.write("[\n" + ",\n".join(json.dumps(r, separators=(",", ":")) for r in data) + "\n]\n")
            else:
                json.dump(data, f, separators=(",", ":"))


if __name__ == "__main__":
    main()
