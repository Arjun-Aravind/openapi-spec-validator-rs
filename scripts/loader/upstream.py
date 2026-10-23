"""Helpers that run openapi-spec-validator's real readers and encode results for the Rust tests.

Run with an environment that has openapi-spec-validator 0.9.0 installed:
    python -I scripts/loader/<script>.py
"""

import io
import os
import struct
import subprocess
import sys
import tempfile

from openapi_spec_validator.readers import read_from_filename, read_from_stdin

BS = chr(92)  # a backslash, kept out of string literals on purpose


def tagged(v):
    """Encode a loaded value with explicit types (floats as IEEE bits, ints as digits)."""
    if v is None:
        return ["n"]
    if v is True or v is False:
        return ["b", v]
    if isinstance(v, int):
        return ["i", str(v)]
    if isinstance(v, float):
        if v != v or v in (float("inf"), float("-inf")):
            return ["n"]  # serde_json::Value cannot hold these; the Rust loader yields null
        return ["f", struct.pack(">d", v).hex()]
    if isinstance(v, str):
        return ["s", v]
    if isinstance(v, list):
        return ["l", [tagged(x) for x in v]]
    if isinstance(v, dict):
        return ["d", [[k, tagged(x)] for k, x in v.items()]]
    raise TypeError(type(v))


def load_file(data: bytes):
    """Upstream's file path: read_from_filename -> urlopen -> PyYAML -> json round trip."""
    fd, path = tempfile.mkstemp(suffix=".yaml")
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(data)
        try:
            spec, _ = read_from_filename(path)
            return {"ok": tagged(spec)}
        except Exception as exc:  # the CLI does print(exc)
            return {"err": str(exc)}
    finally:
        os.unlink(path)


def load_stdin(data: bytes, errors: str):
    """Upstream's stdin path with a replica of CPython's sys.stdin (create_stdio)."""
    fd, path = tempfile.mkstemp(suffix=".yaml")
    saved = sys.stdin
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(data)
        buf = open(path, "rb")
        buf.raw.name = "<stdin>"
        sys.stdin = io.TextIOWrapper(buf, encoding="utf-8", errors=errors, newline="\n")
        try:
            spec, _ = read_from_stdin("-")
            return {"ok": tagged(spec)}
        except Exception as exc:
            return {"err": str(exc)}
        finally:
            sys.stdin.close()
    finally:
        sys.stdin = saved
        os.unlink(path)


def run_cli(data: bytes, mode: str):
    """Run the real CLI. Returns the read error text, or None if reading succeeded."""
    fd, path = tempfile.mkstemp(suffix=".yaml")
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(data)
        cmd = [sys.executable, "-I", "-m", "openapi_spec_validator"]
        env = dict(os.environ)
        if mode == "file":
            prefix = path + ": "
            proc = subprocess.run(cmd + [path], capture_output=True, env=env)
        else:
            # stdin's error handler follows the locale; feed it from a regular file.
            env["LC_ALL"] = "en_US.UTF-8" if mode == "text-strict" else "C"
            prefix = "stdin: "
            with open(path, "rb") as stdin:
                proc = subprocess.run(cmd + ["-"], stdin=stdin, capture_output=True, env=env)
        out = proc.stdout.decode("utf-8", "surrogateescape")
        if out.startswith(prefix):
            return None
        return out[:-1] if out.endswith("\n") else out
    finally:
        os.unlink(path)
