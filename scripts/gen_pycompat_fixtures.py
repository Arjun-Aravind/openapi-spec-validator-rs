"""Generate differential-test fixtures from CPython 3.13 + jsonschema 4.26 + openapi-schema-validator 0.9.0.

Run with an environment that has openapi-spec-validator 0.9.0 installed:
    python -I scripts/gen_pycompat_fixtures.py <output-dir> <specs-dir>

The repository keeps a deterministic sample of the output in
crates/openapi-spec-validator-rs/tests/fixtures/pycompat/.

Every fixture is JSON Lines. Inputs are written with json.dumps and the expectation is computed
from json.loads of that same text, so Rust (serde_json) sees exactly what Python saw.
"""

import io
import json
import math
import os
import pprint
import random
import re
import string
import struct
import sys
import textwrap

import jsonschema
from jsonschema import Draft202012Validator, Draft4Validator
from jsonschema._utils import format_as_index
from jsonschema.exceptions import FormatError, SchemaError, ValidationError
from openapi_schema_validator import _format as oas_format

OUT = sys.argv[1]
SPECS = sys.argv[2]
os.makedirs(OUT, exist_ok=True)

I64_MIN, U64_MAX = -(2**63), 2**64 - 1


def roundtrip(v):
    return json.loads(json.dumps(v))


def writer(name):
    f = open(os.path.join(OUT, name), "w", encoding="utf-8")
    count = [0]

    def write(obj):
        f.write(json.dumps(obj, ensure_ascii=True) + "\n")
        count[0] += 1

    def close():
        f.close()
        print(f"{name}: {count[0]} cases")

    return write, close


def fnv1a64(text):
    h = 0xCBF29CE484222325
    for b in text.encode("utf-8"):
        h ^= b
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{h:016x}"


# ---------------------------------------------------------------------------------------------
# random JSON values
# ---------------------------------------------------------------------------------------------

def nonprintable_boundaries():
    """Code points around every isprintable() transition (excluding surrogates)."""
    pts, prev = [], None
    for c in range(0x110000):
        if 0xD800 <= c <= 0xDFFF:
            continue
        p = chr(c).isprintable()
        if prev is not None and p != prev:
            pts.extend([c - 1, c])
        prev = p
    return [c for c in pts if not 0xD800 <= c <= 0xDFFF]


BOUNDARY_CPS = nonprintable_boundaries()
SPECIAL_CHARS = (
    list("'\"\\\t\n\r\x00\x07\x0b\x0c\x1b\x1c\x1d\x1e\x1f\x7f  \u0085  　​­")
    + ["é", "€", "\U0001d11e", "\U0001f600", "﻿", "", "\U000f0000", "͸",
       "中", "١", "０", " ", " ", " ", " ", " "]
)


def rand_char(rng):
    r = rng.random()
    if r < 0.45:
        return rng.choice(string.ascii_letters + string.digits + " _-./{}:!@#")
    if r < 0.6:
        return rng.choice(SPECIAL_CHARS)
    if r < 0.75:
        c = rng.choice(BOUNDARY_CPS)
        return chr(c)
    while True:
        c = rng.randrange(0x80, 0x110000) if rng.random() < 0.5 else rng.randrange(0x80, 0x3000)
        if not 0xD800 <= c <= 0xDFFF:
            return chr(c)


def rand_str(rng, maxlen=20):
    n = rng.choice([0, 1, 2, 3, 5, 8, 13, maxlen, rng.randrange(0, maxlen + 1)])
    return "".join(rand_char(rng) for _ in range(n))


WORDS = ["the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "API", "schema",
         "a" * 30, "b" * 75, "x", "", "Lorem", "ipsum", "dolor", "sit", "amet,", "été",
         "中文", "\U0001f600"]
SEPS = [" ", " ", " ", "  ", "\t", "\n", "\n\n", "\r\n", "\r", " \n", "\x0b", "\x0c", " ",
        " ", "　", "\x1c", "\x85", " \t "]


def rand_text(rng, nwords=None):
    """Long prose-like strings to exercise pprint's string splitting."""
    n = nwords if nwords is not None else rng.choice([5, 10, 20, 40, 80, rng.randrange(1, 60)])
    parts = []
    for _ in range(n):
        parts.append(rng.choice(WORDS) if rng.random() < 0.85 else rand_str(rng, 12))
        parts.append(rng.choice(SEPS))
    s = "".join(parts)
    if rng.random() < 0.3:
        s = s.strip()
    if rng.random() < 0.2:
        s = rng.choice(SEPS) + s
    return s


def rand_float(rng):
    r = rng.random()
    if r < 0.3:
        while True:
            f = struct.unpack("<d", struct.pack("<Q", rng.getrandbits(64)))[0]
            if math.isfinite(f):
                return f
    if r < 0.5:
        return rng.choice([0.0, -0.0, 1.0, -1.0, 0.5, 0.1, 1e16, 1e15, 1e-4, 1e-5, 123456789.0,
                           1.5e300, 5e-324, 2.2250738585072014e-308, 1.7976931348623157e308,
                           9007199254740993.0, 0.30000000000000004, 1e22, 1e21, 12345678901234567.0])
    if r < 0.8:
        return round(rng.uniform(-1e6, 1e6), rng.randrange(0, 8))
    return rng.uniform(-1, 1) * 10 ** rng.randrange(-30, 30)


def rand_int(rng):
    r = rng.random()
    if r < 0.5:
        return rng.randrange(-1000, 1000)
    if r < 0.8:
        return rng.choice([0, -1, 2**31 - 1, -(2**31), 2**31, -(2**31) - 1, 2**63 - 1, I64_MIN,
                           2**63, U64_MAX, 2**32, 10**15, -(10**18)])
    return rng.randrange(I64_MIN, U64_MAX + 1)


def rand_scalar(rng):
    r = rng.random()
    if r < 0.4:
        return rand_str(rng) if rng.random() < 0.7 else rand_text(rng)
    if r < 0.6:
        return rand_int(rng)
    if r < 0.75:
        return rand_float(rng)
    if r < 0.9:
        return rng.choice([True, False])
    return None


def rand_value(rng, depth=0, max_depth=5, budget=None):
    """Random JSON value with roughly at most `budget` nodes."""
    if budget is None:
        budget = [rng.choice([5, 10, 20, 40, 80])]
    budget[0] -= 1
    r = rng.random()
    if depth >= max_depth or r < 0.35 or budget[0] <= 0:
        return rand_scalar(rng)
    n = min(rng.choice([0, 1, 2, 3, 4, 6, 10, rng.randrange(0, 15)]), budget[0])
    if r < 0.65:
        return [rand_value(rng, depth + 1, max_depth, budget) for _ in range(n)]
    d = {}
    for _ in range(n):
        key = rand_str(rng, 15) if rng.random() < 0.5 else rng.choice(
            ["type", "properties", "items", "$ref", "description", "a", "b", "B", "_", "",
             "x-extension", "default", "enum", "é", "'q'", '"dq"', "it's"])
        d[key] = rand_value(rng, depth + 1, max_depth, budget)
    return d


# ---------------------------------------------------------------------------------------------
# repr / repr_str / repr_float
# ---------------------------------------------------------------------------------------------

def gen_repr(rng):
    write, close = writer("repr.jsonl")
    for _ in range(4000):
        v = roundtrip(rand_value(rng, max_depth=4))
        write({"value": v, "repr": repr(v)})
    close()

    write, close = writer("repr_str.jsonl")
    # every isprintable() boundary, alone and in context
    for c in BOUNDARY_CPS:
        s = chr(c)
        write({"s": s, "repr": repr(s)})
    for c in range(0, 0x200):
        write({"s": chr(c), "repr": repr(chr(c))})
    for _ in range(20000):
        s = rand_str(rng, 40)
        write({"s": s, "repr": repr(s)})
    for s in ["", "'", '"', "'\"", "it's", 'say "hi"', "both ' and \"", "\\", "\\'", "''\"\"",
              "\x00\x7f\x80\x9f\xa0\xad\xffĀ￿\U00010000\U0010ffff"]:
        write({"s": s, "repr": repr(s)})
    close()

    write, close = writer("repr_float.jsonl")
    specials = [0.0, -0.0, math.inf, -math.inf, math.nan, 1e16, 1e15, 9999999999999998.0,
                1e-4, 1e-5, 0.0001, 0.00001, 0.00012345, 1.5e-5, 123.456, 1e100, 1e-100,
                5e-324, 2.2250738585072014e-308, 1.7976931348623157e308, 0.1, 0.2, 0.3,
                1 / 3, 2 / 3, 100.0, 1e22, 1e23, 9007199254740992.0, 4.35, 2.675, 1e16 + 2]
    for k in range(-30, 30):
        for m in (1.0, 9.999999999999999, 1.5, 5.0):
            specials.append(m * 10.0**k)
    for f in specials:
        write({"bits": f"{struct.unpack('<Q', struct.pack('<d', f))[0]:016x}", "repr": repr(f)})
    for _ in range(30000):
        if rng.random() < 0.5:
            bits = rng.getrandbits(64)
        else:
            bits = struct.unpack("<Q", struct.pack("<d", rand_float(rng)))[0]
        f = struct.unpack("<d", struct.pack("<Q", bits))[0]
        if math.isnan(f):
            bits = 0x7FF8000000000000
            f = math.nan
        write({"bits": f"{bits:016x}", "repr": repr(f)})
    for n in range(1, 2000):
        write({"bits": f"{struct.unpack('<Q', struct.pack('<d', float(n)))[0]:016x}", "repr": repr(float(n))})
    close()

    # Bulk: 300k doubles (random bit patterns and short decimal literals) as "hexbits repr" lines.
    with open(os.path.join(OUT, "repr_float_bulk.txt"), "w") as f:
        for _ in range(300000):
            if rng.random() < 0.7:
                bits = rng.getrandbits(64)
            else:
                literal = f"{rng.randrange(1, 10 ** rng.randrange(1, 18))}e{rng.randrange(-30, 30)}"
                bits = struct.unpack("<Q", struct.pack("<d", float(literal)))[0]
            x = struct.unpack("<d", struct.pack("<Q", bits))[0]
            if not math.isnan(x):
                f.write(f"{bits:016x} {x!r}\n")
    print("repr_float_bulk.txt: 300000 lines (minus NaNs)")


# ---------------------------------------------------------------------------------------------
# pformat
# ---------------------------------------------------------------------------------------------

def spec_subtrees(doc, rng, count, max_chars=20000):
    """Sample `count` random subtrees of a JSON document (biased towards mid-sized nodes)."""
    nodes = []

    def walk(v, depth):
        if isinstance(v, (dict, list)):
            nodes.append(v)
            items = v.values() if isinstance(v, dict) else v
            for x in items:
                walk(x, depth + 1)
        elif isinstance(v, str) and len(v) > 40:
            nodes.append(v)

    walk(doc, 0)
    rng.shuffle(nodes)
    picks = []
    for v in nodes:
        if len(json.dumps(v)) <= max_chars:
            picks.append(v)
            if len(picks) == count:
                break
    return picks


def gen_pformat(rng, specs):
    write, close = writer("pformat.jsonl")
    widths = [1, 2, 5, 10, 20, 30, 40, 50, 60, 72, 72, 72, 80, 80, 100, 120]
    for i in range(6000):
        if i % 4 == 0:
            v = rand_text(rng)
            if rng.random() < 0.5:
                v = {rand_str(rng, 6): v} if rng.random() < 0.5 else [v, rand_text(rng)]
        else:
            v = rand_value(rng, max_depth=rng.choice([2, 3, 4, 5, 6]))
        v = roundtrip(v)
        w = rng.choice(widths)
        sd = rng.random() < 0.5
        write({"value": v, "width": w, "sort_dicts": sd, "out": pprint.pformat(v, width=w, sort_dicts=sd)})
    close()

    write, close = writer("pformat_specs.jsonl")
    for path in specs:
        doc = json.load(open(path, encoding="utf-8"))
        for v in spec_subtrees(doc, rng, 400):
            w = rng.choice([72, 72, 80, 40, 120])
            sd = rng.random() < 0.5
            write({"value": v, "width": w, "sort_dicts": sd, "out": pprint.pformat(v, width=w, sort_dicts=sd)})
    close()

    write, close = writer("pformat_docs.jsonl")
    for path in specs:
        doc = json.load(open(path, encoding="utf-8"))
        for w, sd in [(72, False), (80, True)]:
            out = pprint.pformat(doc, width=w, sort_dicts=sd)
            write({"spec": os.path.basename(path), "width": w, "sort_dicts": sd,
                   "chars": len(out), "fnv1a64": fnv1a64(out)})
    close()


# ---------------------------------------------------------------------------------------------
# format_as_index / ValidationError.__str__
# ---------------------------------------------------------------------------------------------

def path_items(seq):
    return [{"index": x} if isinstance(x, int) else {"key": x} for x in seq]


def error_case(e):
    return {
        "kind": "schema" if isinstance(e, SchemaError) else "validation",
        "message": e.message,
        "validator": e.validator,
        "schema_path": path_items(e.relative_schema_path),
        "schema": e.schema,
        "instance_path": path_items(e.relative_path),
        "instance": e.instance,
        "str": str(e),
    }


SCHEMAS = [
    {"type": "object", "properties": {"a": {"type": "integer"}, "b": {"type": "string", "maxLength": 3}},
     "required": ["a"], "additionalProperties": False},
    {"type": "array", "items": {"type": "object", "properties": {"name": {"enum": ["x", "y"]}}}},
    {"oneOf": [{"type": "string"}, {"type": "integer", "minimum": 10}]},
    {"anyOf": [{"type": "null"}, {"type": "array", "minItems": 2}]},
    {"type": "object", "patternProperties": {"^x-": {"type": "boolean"}},
     "properties": {"description": {"type": "string", "pattern": "^[A-Z]"}}},
    {"type": "string", "format": "date"},
    {"const": {"k": [1, 2, 3]}},
    {"type": "object", "properties": {"nested": {"type": "object", "properties": {
        "deep": {"type": "array", "items": {"type": "number", "multipleOf": 0.5}}}}}},
    {"not": {"type": "object"}},
    {"type": "object", "minProperties": 2, "propertyNames": {"maxLength": 4}},
    {"type": "object", "dependentRequired": {"a": ["b", "c"]}},
    {"items": [{"type": "string"}], "type": "array"},
    False,
]


def gen_errors(rng, specs):
    write, close = writer("format_as_index.jsonl")
    for _ in range(1500):
        container = rng.choice(["schema", "instance", "metaschema", "x", ""])
        idx = [rng.randrange(0, 10**6) if rng.random() < 0.4 else rand_str(rng, 12)
               for _ in range(rng.choice([0, 1, 2, 3, 6]))]
        write({"container": container, "indices": path_items(idx), "out": format_as_index(container, idx)})
    close()

    write, close = writer("validation_error.jsonl")
    # Real errors from jsonschema validating random instances.
    for _ in range(2500):
        schema = rng.choice(SCHEMAS)
        if rng.random() < 0.5:
            instance = roundtrip(rand_value(rng, max_depth=4))
        else:
            instance = roundtrip(rng.choice([
                {"a": "x", "b": "long string" * rng.randrange(1, 20)},
                [{"name": rand_str(rng)}, {"name": "x"}, 3],
                {"nested": {"deep": [1, 0.25, rand_float(rng), "s"]}},
                {"x-a": 1, "description": rand_text(rng)},
                {"abcdef": 1}, "2020-13-45", [1], {"a": 1}, {"k": [1, 2, 4]},
            ]))
        cls = Draft4Validator if schema is SCHEMAS[11] else Draft202012Validator
        v = cls(schema, format_checker=Draft202012Validator.FORMAT_CHECKER)
        for e in list(v.iter_errors(instance))[:3]:
            write(error_case(e))
            for sub in e.context[:2]:
                write(error_case(sub))
    # Real SchemaErrors.
    for bad in [{"type": 12}, {"properties": {"a": {"minimum": "x"}}}, {"required": "a"},
                {"enum": 3}, {"items": {"type": ["string", 7]}}]:
        try:
            Draft202012Validator.check_schema(bad)
        except SchemaError as e:
            write(error_case(e))
    # Synthetic errors: odd messages (dedent quirks), big schemas / instances from the specs.
    messages = ["simple", "", "two\nlines", "tab\tinside", "  leading spaces", "\tleading tab",
                "line\n            indented 12", "line\n                indented 16",
                "trailing spaces   ", "trailing newline\n", "x\n\t\ty", "x\n  \ny", "a\r\nb",
                "é é", "x\n" + " " * 12 + "\ty"]
    docs = [json.load(open(p, encoding="utf-8")) for p in specs]
    nodes = []
    for d in docs:
        nodes.extend(spec_subtrees(d, rng, 150, 4000))
    for i in range(1500):
        msg = rng.choice(messages) if rng.random() < 0.5 else rand_text(rng, rng.randrange(1, 8))
        validator = rng.choice(["type", "required", "$ref", "x'y", rand_str(rng, 6)])
        spath = [rng.randrange(0, 5) if rng.random() < 0.3 else rand_str(rng, 8)
                 for _ in range(rng.randrange(0, 5))]
        ipath = [rng.randrange(0, 5) if rng.random() < 0.3 else rand_str(rng, 8)
                 for _ in range(rng.randrange(0, 4))]
        schema = rng.choice(nodes) if rng.random() < 0.5 else roundtrip(rand_value(rng, max_depth=4))
        instance = rng.choice(nodes) if rng.random() < 0.3 else roundtrip(rand_value(rng, max_depth=4))
        cls = SchemaError if i % 7 == 0 else ValidationError
        e = cls(msg, validator=validator, validator_value=None, instance=instance, schema=schema,
                schema_path=spath, path=ipath)
        write(error_case(e))
    close()


# ---------------------------------------------------------------------------------------------
# string.Formatter().parse
# ---------------------------------------------------------------------------------------------

def gen_formatter(rng, specs):
    write, close = writer("formatter.jsonl")
    fmt = string.Formatter()

    def case(s):
        try:
            names = [item[1] for item in fmt.parse(s)]
            write({"s": s, "names": names, "error": None})
        except ValueError as e:
            write({"s": s, "names": None, "error": str(e)})

    for path in specs:
        doc = json.load(open(path, encoding="utf-8"))
        for url in doc.get("paths", {}):
            case(url)
    alphabet = list("{}!:[].ab/") + ["{", "}", "a", "b", "/", "r", "s", "0", " ", "é", "\U0001f600"]
    for _ in range(6000):
        n = rng.choice([0, 1, 2, 3, 4, 5, 6, 8, 10, 15, rng.randrange(0, 25)])
        case("".join(rng.choice(alphabet) for _ in range(n)))
    templates = ["/pets/{petId}", "/a/{b}/c/{d}", "/{x}", "{}", "{{}}", "{{x}}", "/{a.b}", "/{a[0]}",
                 "/{a!r}", "/{a!s:>10}", "/{a:{b}}", "/{a:{b}{c}}", "/{a:{{}}}", "/{a[}]}", "/{a[{]}",
                 "/{a!}", "/{a!}}", "/{a!r", "/{a!rx}", "/{a:", "/{a!", "/{", "/}", "/{a", "/{a[", "/{a[b]",
                 "/{a]}", "}}{{", "/{a}}", "/{{a}", "/{0}", "/{ }", "/{a b}", "/{a:b:c}", "/{a!r:}",
                 "/{!r}", "/{:}", "/{a!:}", "/{a!!}", "/{a!{}", "/{a:}}", "/{a:{}}"]
    for t in templates:
        case(t)
        for _ in range(20):
            i = rng.randrange(0, len(t) + 1)
            case(t[:i] + rng.choice(alphabet) + t[i:])
            j = rng.randrange(0, max(len(t), 1))
            case(t[:j] + t[j + 1:])
    close()


# ---------------------------------------------------------------------------------------------
# format checkers
# ---------------------------------------------------------------------------------------------

CHECKERS = {
    "oas30": oas_format.oas30_format_checker,
    "oas31": oas_format.oas31_format_checker,
    "oas32": oas_format.oas32_format_checker,
}
UNI_DIGITS = ["٠", "١", "٥", "۳", "१", "０", "１", "５",
              "\U0001d7ce", "\U0001d7d9", "၀", "๑"]
UNI_SPACES = [" ", " ", "　", "\u0085", " ", " ", "\x1c", "\x1f", "\x0b"]


def mutate(rng, s, alphabet):
    s = list(s)
    for _ in range(rng.choice([1, 1, 1, 2, 3])):
        op = rng.random()
        ch = rng.choice(alphabet)
        if op < 0.35 and s:
            s[rng.randrange(len(s))] = ch
        elif op < 0.7:
            s.insert(rng.randrange(len(s) + 1), ch)
        elif s:
            del s[rng.randrange(len(s))]
    return "".join(s)


def fmt_samples(rng, fmt):
    """Format-specific strings: valid seeds, edge cases, and random mutations."""
    digits = string.digits
    if fmt == "date":
        seeds = ["2020-01-01", "2020-02-29", "2021-02-29", "2000-02-29", "1900-02-29", "0000-01-01",
                 "0001-01-01", "9999-12-31", "2020-13-01", "2020-00-10", "2020-04-31", "2020-01-00",
                 "2020-1-01", "2020-01-01\n", "20200101", "2020/01/01", "2020-01-01T00:00:00Z",
                 " 2020-01-01", "٢٠٢٠-01-01", "2020-01-32", "2019-06-31", "2400-02-29",
                 "2100-02-29"]
        extra = [f"{rng.randrange(0, 10000):04d}-{rng.randrange(0, 14):02d}-{rng.randrange(0, 33):02d}"
                 for _ in range(80)]
        alpha = list(digits + "-T :/") + UNI_DIGITS[:3] + ["\n"]
    elif fmt == "date-time":
        seeds = ["2020-01-01T00:00:00Z", "2020-01-01t00:00:00z", "2020-01-01T23:59:59+23:59",
                 "2020-01-01T24:00:00Z", "2020-02-29T12:00:00.123456Z", "2021-02-29T12:00:00Z",
                 "0000-01-01T00:00:00Z", "2020-01-01T00:00:00.Z", "2020-01-01T00:00:00+24:00",
                 "2020-01-01T00:00:00-00:60", "2020-01-01T00:00:00Z\n", "2020-01-01T00:00:00Z\n\n",
                 "2020-01-01 00:00:00Z", "2020-01-01T00:00:60Z", "2020-01-01T00:00:00", "2020-01-01T00:00Z",
                 "2020-01-01T00:00:00+0100", "2020-12-31T23:59:59.999999999999-12:00", "ß2020",
                 "2020-01-01T00:00:00ź", "2020-06-31T00:00:00Z", "1900-02-29T00:00:00Z",
                 "2000-02-29T00:00:00Z", "2020-01-01ıT00:00:00Z", "2020-01-01T00:00:00.5+05:30"]
        extra = [f"{rng.randrange(0, 10000):04d}-{rng.randrange(0, 14):02d}-{rng.randrange(0, 33):02d}"
                 f"{rng.choice('Tt ')}{rng.randrange(0, 26):02d}:{rng.randrange(0, 62):02d}:"
                 f"{rng.randrange(0, 62):02d}{rng.choice(['', '.1', '.123456', '.'])}"
                 f"{rng.choice(['Z', 'z', '+01:00', '-23:59', '+24:00', '', '+1:00'])}" for _ in range(80)]
        alpha = list(digits + "-T:Z+.tz \n")
    elif fmt == "time":
        seeds = ["00:00:00", "23:59:59", "24:00:00", "12:60:00", "12:00:60", "12:00:61", "12:00:62",
                 "1:2:3", " 1:02:03", "  1:02:03", "01:02:03Z", "01:02:03.5", "01:02:03+01:00",
                 "12:30:456", "12:00:65", "12:00:7x", "2:3:4", "20:3:4", "29:00:00", "1:60:00",
                 "١٢:٣٤:٥٦", "0٥:00:00", "٥:00:00", "１:00:00",
                 "12:00", "12-00-00", "", ":", "::", "12:00:00\n", "12:00:00 ", " 12:00:00",
                 " 5:00:00", "\t5:00:00", "6:00:61", "10:00:\U0001d7d7", "1\U0001d7ce:00:00"]
        extra = [f"{rng.randrange(0, 30)}:{rng.randrange(0, 70)}:{rng.randrange(0, 70)}" for _ in range(60)]
        extra += [f"{rng.randrange(0, 30):02d}:{rng.randrange(0, 70):02d}:{rng.randrange(0, 70):02d}"
                  for _ in range(40)]
        alpha = list(digits + ": .Zx") + UNI_DIGITS + [" "]
    elif fmt == "ipv4":
        seeds = ["0.0.0.0", "127.0.0.1", "255.255.255.255", "256.0.0.1", "01.2.3.4", "1.2.3", "1.2.3.4.5",
                 "", "1..2.3", "1.2.3.4/24", "1.2.3.04", "1.2.3.0", "1.2.3.1000", "a.b.c.d", " 1.2.3.4",
                 "1.2.3.4 ", "١.2.3.4", "1.2.3.４", "::1", "1.2.3.-4", "+1.2.3.4", "1.2.3.4\n",
                 "999.1.1.1", "0x1.2.3.4", "00.0.0.0", "0.0.0.00"]
        extra = [".".join(str(rng.randrange(0, 300)) for _ in range(rng.choice([3, 4, 4, 4, 5])))
                 for _ in range(80)]
        alpha = list(digits + ". /x") + ["١", "é"]
    elif fmt == "ipv6":
        seeds = ["::", "::1", "1::", "1:2:3:4:5:6:7:8", "1:2:3:4:5:6:7::", "::2:3:4:5:6:7:8",
                 "1:2:3:4:5:6:7:8:9", "1::2::3", ":1:2:3:4:5:6:7", "1:2:3:4:5:6:7:", "1:2:3:4:5:6:7",
                 "fe80::1%eth0", "fe80::1%", "fe80::1%a%b", "::ffff:1.2.3.4", "::ffff:1.2.3", "::1.2.3.256",
                 "1:2:3:4:5:6:1.2.3.4", "1:2:3:4:5:6:7:1.2.3.4", "12345::", "g::", "::/64", "",
                 "1:2:3:4:5:6:7:8::", "::1:2:3:4:5:6:7:8", "1::2:3:4:5:6:7:8", ":::", "::::", "1:::2",
                 "a" * 46, "1:" * 30 + "1", "1:" * 60, "ABCD:EF01:2345:6789:ABCD:EF01:2345:6789",
                 "١::", "::１", " ::1", "::1 ", "1:2:3:4:5:6:7:8%25", "::01234",
                 "::ffff:01.2.3.4", "1:2", "1", ":", "1.2.3.4", "::1.2.3.4.5", "[::1]", "%eth0",
                 "1:2:3:4:5:6:7:8:", ":1:2:3:4:5:6:7:8", "::1:2:3:4:5:6:7", "1:2:3:4:5:6:7::8"]
        extra = []
        for _ in range(80):
            n = rng.randrange(1, 10)
            hextets = [format(rng.randrange(0, 0x10000), "x")[: rng.randrange(0, 5)] for _ in range(n)]
            s = ":".join(hextets)
            if rng.random() < 0.3:
                s += "%" + rng.choice(["eth0", "", "1"])
            extra.append(s)
        alpha = list("0123456789abcdefABCDEFg:.%/ ") + ["١"]
    elif fmt == "uuid":
        u = "12345678-1234-5678-1234-567812345678"
        seeds = [u, u.upper(), "{" + u + "}", "urn:uuid:" + u, u.replace("-", ""), "1234_678-1234-5678-1234-567812345678",
                 "0x345678-1234-5678-1234-567812345678", "-2345678-1234-5678-1234-567812345678",
                 "-0000000-0000-0000-0000-000000000000", "+2345678-1234-5678-1234-567812345678",
                 " 2345678-1234-5678-1234-567812345678", "1234567 -1234-5678-1234-567812345678",
                 "12345678-1234-5678-1234-56781234567g", "١٢345678-1234-5678-1234-567812345678",
                 "１２345678-1234-5678-1234-567812345678", " 2345678-1234-5678-1234-567812345678",
                 "12345678-1234-5678-1234-56781234567 ", "12345678-1234-5678-1234-5678123456789",
                 "12345678-1234-5678-1234-5678123456", "12345678123456781234567812345678----",
                 "{{12345678-1234-5678-1234-567812345678}}", "uuid:12345678-1234-5678-1234-567812345678",
                 "urn:12345678-1234-5678-1234-567812345678", "1234urn:5678-1234-5678-1234-567812345678",
                 "12345678-1234-5678-1234-567812345678}", "{12345678-1234-5678-1234-567812345678",
                 "12345678-12345678-1234-1234-567812345678", "0X345678-1234-5678-1234-567812345678",
                 "0x_45678-1234-5678-1234-567812345678", "1__45678-1234-5678-1234-567812345678",
                 "_2345678-1234-5678-1234-567812345678", "1234567_-1234-5678-1234-567812345678",
                 "\x002345678-1234-5678-1234-567812345678", "\x1c2345678-1234-5678-1234-567812345678",
                 "é" * 32, "é" * 36, "\U0001f600" + "2345678-1234-5678-1234-567812345678",
                 "\t2345678-1234-5678-1234-56781234567\n", "00000000-0000-0000-0000-000000000000",
                 "ffffffff-ffff-ffff-ffff-ffffffffffff", "-fffffff-ffff-ffff-ffff-ffffffffffff",
                 "\\" * 32, "'" * 32 + "\"", "\x00" * 40]
        extra = []
        for _ in range(60):
            h = "".join(rng.choice("0123456789abcdefABCDEF") for _ in range(32))
            extra.append(f"{h[:8]}-{h[8:12]}-{h[12:16]}-{h[16:20]}-{h[20:]}")
        alpha = list("0123456789abcdef-{}_ xXgu:rn") + UNI_DIGITS[:4] + UNI_SPACES[:3] + ["+", "-"]
    elif fmt == "byte":
        seeds = ["", "QQ==", "QUI=", "QUJD", "QQ", "QQ=", "Q", "Q===", "QQ===", "=QQ=", "QQ==QQ==", "QUJD=",
                 "QU=D", "QUJD\n", "QU JD", "QUJ-", "QUJ_", "QUJ+", "QUJ/", "aGVsbG8gd29ybGQ=", "éQQ==",
                 "QQéé==", "€", "\U0001f600xé", "aébé", "====", "Q=Q=",
                 "QUJDRA", "QUJDRA=", "QUJDRA==", "QUJDRA===", "QUJDR", "QUJDR=", "QUJDR==", "AAAA====",
                 "\x00", "AA==\x00", "AAA\x7f"]
        extra = ["".join(rng.choice(string.ascii_letters + string.digits + "+/") for _ in range(rng.randrange(0, 13)))
                 + "=" * rng.randrange(0, 4) for _ in range(80)]
        alpha = list(string.ascii_letters[:6] + "0189+/=-_ \n") + ["é", "€", "\U0001f600"]
    elif fmt in ("email", "idn-email"):
        seeds = ["a@b", "@", "", "plain", "x@", "@y", "é@é", "a@@b", " @ "]
        extra = [rand_str(rng, 10) for _ in range(40)]
        alpha = list("ab@. ") + ["é"]
    elif fmt == "regex":
        seeds = ["^a$", "(", ")", "[", "a{2,1}", "\\d+", "(?P<x>a)", "(?<x>a)", "*", "a**", "\\", "[a-]",
                 "[z-a]", "(?i)a", "a(?i)", "\\p{L}", "\\u0041", "\\x4", "(?P=x)", "",
                 "a{4294967295}", "a{1,99999999999999999999}", "(" * 3000 + ")" * 3000]
        extra = [rand_str(rng, 8) for _ in range(40)]
        alpha = list("()[]{}*+?\\^$|.a1,-")
    else:
        seeds = ["", "x", "1", "1.5", "abc", "é"]
        extra = [rand_str(rng, 10) for _ in range(30)]
        alpha = list("abc123")
    out = list(seeds) + extra
    base = seeds + extra
    for _ in range(200):
        out.append(mutate(rng, rng.choice(base), alpha))
    return out


GENERIC = [None, True, False, 0, 1, -1, 2**31 - 1, 2**31, -(2**31), -(2**31) - 1, 2**63 - 1, 2**63,
           I64_MIN, U64_MAX, 0.0, -0.0, 1.0, 1.5, -2.5, 1e300, [], {}, [1, "a"], {"a": "b"}, "", "a@b"]


INT_EDGES = sorted({b + k for b in (0, 2**31, -(2**31), 2**32, 2**63, -(2**63), U64_MAX) for k in range(-3, 4)
                    if I64_MIN <= b + k <= U64_MAX})


def check(checker, fmt, instance, set_name):
    func_raises = checker.checkers.get(fmt)
    try:
        checker.check(instance, fmt)
    except FormatError as e:
        return {"outcome": "invalid", "message": e.message,
                "cause": None if e.cause is None else str(e.cause)}
    except Exception as e:  # propagates out of jsonschema validation
        return {"outcome": "crash", "message": str(e), "cause": None, "exc": type(e).__name__}
    if func_raises is None:
        return {"outcome": "unknown", "message": None, "cause": None}
    return {"outcome": "valid", "message": None, "cause": None}


def gen_formats(rng):
    write, close = writer("formats.jsonl")
    formats = ["binary", "byte", "date", "date-time", "double", "email", "float", "idn-email", "int32",
               "int64", "ipv4", "ipv6", "password", "regex", "time", "uuid",
               # not registered anywhere in this environment
               "uri", "hostname", "duration", "foo", "", "DATE", "int16", "uri-reference", "json-pointer"]
    for set_name in ("oas30", "oas31", "oas32"):
        assert sorted(CHECKERS[set_name].checkers) == sorted(
            f for f in formats[:16] if set_name == "oas30" or f not in ("binary", "byte")), set_name
    for fmt in formats:
        values = list(GENERIC) + fmt_samples(rng, fmt)
        values += INT_EDGES + [rand_int(rng) for _ in range(30)] + [rand_float(rng) for _ in range(20)]
        values += [roundtrip(rand_value(rng, max_depth=2)) for _ in range(20)]
        values += [rand_str(rng, 12) for _ in range(20)]
        seen = set()
        for v in values:
            v = roundtrip(v)
            key = json.dumps(v)
            if key in seen:
                continue
            seen.add(key)
            regex_valid = None  # re.compile: True / False (re.error) / "<str(exc)>" (anything else)
            if isinstance(v, str):
                try:
                    re.compile(v)
                    regex_valid = True
                except re.error:
                    regex_valid = False
                except Exception as e:
                    regex_valid = str(e)
            for set_name, checker in CHECKERS.items():
                res = check(checker, fmt, v, set_name)
                write({"set": set_name, "format": fmt, "instance": v, "regex_valid": regex_valid, **res})
    close()


def gen_arbitrary_precision():
    """Numbers serde_json can only carry faithfully with `arbitrary_precision` (raw JSON text)."""
    literals = ["-0", "0", "18446744073709551615", "18446744073709551616", "-9223372036854775808",
                "-9223372036854775809", "170141183460469231731687303715884105727",
                "170141183460469231731687303715884105728", "-" + "9" * 45, "1" + "0" * 30, "1e400", "-1e400",
                "1E400", "1.5e999", "-0.0", "1.0", "1e16", "2.5E-3", "123456789012345678901234567890.5"]
    path = os.path.join(OUT, "numbers_ap.jsonl")
    n = 0
    with open(path, "w") as f:
        for i, lit in enumerate(literals):
            texts = [lit, f"[{lit}, {{\"k\": {lit}}}]", f"{{\"a\": [{lit}, \"x\"], \"b\": {lit}}}"]
            for text in texts:
                v = json.loads(text)
                formats = {}
                for set_name, checker in CHECKERS.items():
                    for fmt in ("int32", "int64", "float", "double", "date"):
                        res = check(checker, fmt, v, set_name)
                        formats[f"{set_name}/{fmt}"] = [res["outcome"], res["message"]]
                f.write('{"text": %s, "value": %s, "repr": %s, "pformat": %s, "formats": %s}\n' % (
                    json.dumps(text), text, json.dumps(repr(v)),
                    json.dumps(pprint.pformat(v, width=20, sort_dicts=False)), json.dumps(formats)))
                n += 1
    print(f"numbers_ap.jsonl: {n} cases")


def main():
    rng = random.Random(20261023)
    specs = sorted(os.path.join(SPECS, f) for f in os.listdir(SPECS) if f.endswith(".json"))
    gen_repr(rng)
    gen_pformat(rng, specs)
    gen_errors(rng, specs)
    gen_formatter(rng, specs)
    gen_formats(rng)
    gen_arbitrary_precision()


if __name__ == "__main__":
    main()
