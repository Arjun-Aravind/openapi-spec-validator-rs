"""Measure how deep upstream's Python stack gets in each phase of a validation.

For every document given, runs openapi-spec-validator's command line in-process under a
profiler and prints, as JSON lines, one record per phase that recurses through a
meta-schema: the document check (``doc``) and each schema's dialect check (``meta``,
with the schema's location). ``root`` is the depth of the phase's ``iter_errors`` frame
(the command line's ``<module>`` frame is depth 1) and ``max`` the deepest frame reached
under it. The frame model in ``crates/openapi-spec-validator-rs/src/frames.rs`` is fitted
to these numbers (``scripts/frames/compare.py``).

Usage:
    python scripts/frames/measure.py FILE...   (with upstream and the jsonschema backend)
"""

import contextlib
import io
import json
import os
import sys

os.environ["OPENAPI_SPEC_VALIDATOR_SCHEMA_VALIDATOR_BACKEND"] = "jsonschema"
CHAINS = bool(os.environ.get("CHAINS"))

from openapi_spec_validator.__main__ import main  # noqa: E402


def depth_of(frame):
    n = 0
    while frame is not None:
        n += 1
        frame = frame.f_back
    return n


def measure(path):
    records = []
    roots = []  # (frame, record)

    def profile(frame, event, arg):
        if event == "call":
            code = frame.f_code
            if code.co_name == "iter_errors" and code.co_filename.endswith(
                os.path.join("jsonschema", "validators.py")
            ):
                caller = frame.f_back.f_code
                record = None
                if caller.co_name == "iter_errors" and "openapi_spec_validator" in caller.co_filename:
                    record = {"phase": "doc"}
                elif caller.co_name == "check_openapi_schema":
                    schema = frame.f_back.f_back.f_back.f_locals.get("schema")
                    parts = (str(p).replace("~", "~0").replace("/", "~1") for p in getattr(schema, "parts", ()))
                    record = {"phase": "meta", "at": "/" + "/".join(parts)}
                if record is not None:
                    record["root"] = depth_of(frame)
                    record["max"] = record["root"]
                    records.append(record)
                    roots.append((frame, record))
                    return
            if roots:
                record = roots[-1][1]
                d = depth_of(frame)
                if d > record["max"]:
                    record["max"] = d
                    if CHAINS:
                        chain, f = [], frame
                        for _ in range(d - record["root"] + 1):
                            chain.append(f"{f.f_code.co_name}@{os.path.basename(f.f_code.co_filename)}:{f.f_lineno}")
                            f = f.f_back
                        record["chain"] = list(reversed(chain))
        elif event == "return" and roots and roots[-1][0] is frame:
            roots.pop()

    sys.setprofile(profile)
    try:
        with contextlib.redirect_stdout(io.StringIO()):
            main([path])
    except SystemExit:
        pass
    except RecursionError:
        records.append({"phase": "crash"})
    finally:
        sys.setprofile(None)
    return records


if __name__ == "__main__":
    sys.setrecursionlimit(100_000)
    for path in sys.argv[1:]:
        print(json.dumps({"file": path, "phases": measure(path)}))
