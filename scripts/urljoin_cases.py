"""Generate the `urljoin` / `urldefrag` fixtures the Rust port is tested against.

Writes crates/openapi-spec-validator-rs/tests/fixtures/{urljoin,urldefrag}.json with the
results of CPython's own `urllib.parse` functions, which upstream uses to resolve `$ref`s.
Python 3.14 changed both; run with 3.14 or later, it writes urljoin-3.14.json and
urldefrag-3.14.json instead.
Usage: python3.13 scripts/urljoin_cases.py; python3.14 scripts/urljoin_cases.py
"""

import json
import pathlib
import sys
from urllib.parse import urldefrag, urljoin

BASES = [
    "", "file:///x/spec.json", "file:///a%20b/spec.yaml", "file:///x/", "file:///x",
    "http://h/a/b.json", "http://h/a/b.json?q=1#f", "http://h/a/", "http://h", "http://h/",
    "https://H:8443/A/B;p?q", "http://u:p@h:80/a/b", "http://[::1]:8/a/b.json",
    "http://h/a;p/b;q?z", "HTTP://h/x", "ftp://h/a/b", "urn:isbn:123", "/a/b", "a/b",
    "http://h/sub/../x", "http://h/a/b/", "http://h/a\tb/c", " http://h/x", "http://h/x ",
]
REFS = [
    "", "#", "#frag", "#/components/schemas/A", "?q=1", "?", ";p", "c;d", "x", "x.json",
    "./x", "../x", "../../x", "../../../x", "./", "../", ".", "..", "/x", "/", "//other/x",
    "//other", "sub/../c.json", "sub/./../c.json", "a b.json", "caf\u00e9.json", "tab\t.json",
    "nl\n.json", " lead.json", "x#f", "x?q#f", "x?q=1&r=2", "http://h/x", "http://h/sub/../x",
    "http://h/./x/.././y", "http://H:80/A%2fb", "HTTP://h/x", "https://h/x", "file:///y.json",
    "file:y.json", "file://h/y", "urn:x", "mailto:a@b", "http:", "http:x", "http:/x",
    "http://", "//", "///x", "x/", "x//y", "/x//y", "x;p?q", "http://u:p@h/x",
    "http://[::1]/x", "http://[::1/x", "http://]/x", "c:/x", "c:x", "data:text/plain,hi",
    "\u00e9", "%41", "%zz", "a?b#c?d", "a#b#c", "a?b?c", "..\\x", "x\\y",
]


def result(base: str, ref: str):
    try:
        return urljoin(base, ref)
    except ValueError:
        return None


def main() -> None:
    out = pathlib.Path(__file__).resolve().parent.parent / "crates/openapi-spec-validator-rs/tests/fixtures"
    joins = [[b, r, result(b, r)] for b in BASES for r in REFS]
    suffix = "-3.14" if sys.version_info >= (3, 14) else ""
    (out / f"urljoin{suffix}.json").write_text(json.dumps(joins, ensure_ascii=False, indent=0) + "\n")
    urls = sorted({r for r in REFS} | {b for b in BASES} | {j[2] for j in joins if j[2]})
    defrags = []
    for url in urls:
        try:
            defrags.append([url, list(urldefrag(url))])
        except ValueError:
            defrags.append([url, None])
    (out / f"urldefrag{suffix}.json").write_text(json.dumps(defrags, ensure_ascii=False, indent=0) + "\n")
    print(f"{len(joins)} urljoin cases, {len(defrags)} urldefrag cases")


if __name__ == "__main__":
    main()
