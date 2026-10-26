"""Generate the `urljoin` / `urldefrag` fixtures the Rust port is tested against.

Writes crates/openapi-spec-validator-rs/tests/fixtures/{urljoin,urldefrag}-3.X.json with the
results of CPython's own `urllib.parse` functions, which upstream uses to resolve `$ref`s,
for the Python 3.X running it. They differ between versions, so run it with the latest
patch release of each version from 3.10 on (for example in the python:3.X-slim images).
A `ValueError` is recorded as {"ValueError": message}.
Usage: python3.X scripts/urljoin_cases.py
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
    "rtsps://h/a/b", "itms-services://h/a/b", "1x:/a/b",
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
    # Bracketed hosts (`_check_bracketed_netloc`) and schemes.
    "http://[::1]x/a", "http://a[::1]/a", "http://[::1]]/a", "http://[[::1]/a",
    "http://[::1]:80/a", "http://[::1]:x/a", "http://u@[::1]/a", "http://u[x]@h/a",
    "http://[::1%eth0]/a", "http://[1.2.3.4]/a", "http://[host]/a", "http://[]/a",
    "http://[v1.x]/a", "http://[V1.x]/a", "http://[v1.]/a", "http://[vg.x]/a", "http://[v.x]/a",
    "1x:y", "+x:y", "rtsps://h/y", "itms-services://h/y",
]


def result(base: str, ref: str):
    try:
        return urljoin(base, ref)
    except ValueError as error:
        return {"ValueError": str(error)}


def main() -> None:
    out = pathlib.Path(__file__).resolve().parent.parent / "crates/openapi-spec-validator-rs/tests/fixtures"
    joins = [[b, r, result(b, r)] for b in BASES for r in REFS]
    suffix = "-%d.%d" % sys.version_info[:2]
    (out / f"urljoin{suffix}.json").write_text(json.dumps(joins, ensure_ascii=False, indent=0) + "\n")
    urls = sorted({r for r in REFS} | {b for b in BASES} | {j[2] for j in joins if isinstance(j[2], str)})
    defrags = []
    for url in urls:
        try:
            defrags.append([url, list(urldefrag(url))])
        except ValueError as error:
            defrags.append([url, {"ValueError": str(error)}])
    (out / f"urldefrag{suffix}.json").write_text(json.dumps(defrags, ensure_ascii=False, indent=0) + "\n")
    print(f"{len(joins)} urljoin cases, {len(defrags)} urldefrag cases")


if __name__ == "__main__":
    main()
