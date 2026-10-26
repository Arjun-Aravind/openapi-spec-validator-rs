"""Hostile inputs and environments: what upstream survives, this package survives too."""

from __future__ import annotations

import io
import os
import subprocess
import sys
import textwrap
from pathlib import Path
from typing import Any

import openapi_spec_validator_rs as osv
import pytest
from openapi_spec_validator_rs import SpecReadError, handlers

upstream_readers = pytest.importorskip("openapi_spec_validator.readers")


class Endless(io.RawIOBase):
    """A stream that never ends: `prefix`, then `unit` forever."""

    def __init__(self, prefix: bytes, unit: bytes) -> None:
        self.prefix, self.unit, self.served = prefix, unit, 0

    def readable(self) -> bool:
        return True

    def readinto(self, buffer: Any) -> int:
        n = len(buffer)
        data = (self.prefix + self.unit * n)[:n]
        self.prefix = self.prefix[n:]
        buffer[:n] = data
        self.served += n
        return n


@pytest.mark.parametrize(
    "prefix", [b"", b"\xef\xbb\xbf", b"\xff\xfe", b"\xfe\xff"], ids=["utf-8", "utf-8-bom", "utf-16-le", "utf-16-be"]
)
def test_endless_unreadable_streams_stop_early(prefix: bytes) -> None:
    stream = Endless(prefix, b"\0")
    data = handlers.read_stream(stream)
    assert len(data) <= len(prefix) + 64 * 1024
    # Standard input is decoded as UTF-8 whatever its first bytes are.
    assert len(handlers.read_stream(Endless(prefix, b"\0"), text=True)) <= len(prefix) + 64 * 1024


@pytest.mark.parametrize(
    "content",
    [
        b"\0" * 100_000,
        b"\xff\xfe" + b"\0" * 100_000,
        b"\xfe\xff" + "a: 1\n".encode("utf-16-be") + b"\0\x07",
        b"\xff\xfe" + b"\x00\xdc" * 10,
        b"openapi: 3.0.3\n\xff\xff",
    ],
    ids=["nul", "utf-16-nul", "utf-16-be-bell", "lone-surrogate", "invalid-utf-8"],
)
def test_unreadable_files_fail_as_upstream_fails(tmp_path: Path, content: bytes) -> None:
    path = tmp_path / "spec.yaml"
    path.write_bytes(content)
    with pytest.raises(Exception) as expected:
        upstream_readers.read_from_filename(str(path))
    with pytest.raises(SpecReadError) as actual:
        osv.validate_path(path)
    assert str(actual.value) == str(expected.value)


def test_clean_utf16_files_are_read_whole(tmp_path: Path, make_spec) -> None:
    import json

    path = tmp_path / "spec.json"
    path.write_bytes(b"\xff\xfe" + json.dumps(make_spec(**{"x-pad": "x" * 200_000})).encode("utf-16-le"))
    osv.validate_path(path)


class MutatingRepr:
    """A `repr()` that changes the document it sits in."""

    def __init__(self, spec: dict[str, Any]) -> None:
        self.spec = spec

    def __repr__(self) -> str:
        self.spec["x-added"] = len(self.spec)
        return "MutatingRepr()"


class RaisingRepr:
    def __repr__(self) -> str:
        raise ValueError("no repr")


def test_repr_that_mutates_the_document(make_spec) -> None:
    spec = make_spec()
    spec["x-m"] = MutatingRepr(spec)
    osv.validate(spec)


def test_repr_that_raises(make_spec) -> None:
    # Upstream only asks for the repr when a message includes it.
    osv.validate(make_spec(**{"x-bad": RaisingRepr()}))
    with pytest.raises(ValueError, match="^no repr$"):
        osv.validate(make_spec(info={"title": RaisingRepr(), "version": "1"}))
    with pytest.raises(ValueError, match="^no repr$"):
        osv.validate(make_spec(info={"title": [RaisingRepr(), 1], "version": "1"}))


def test_dict_changed_during_conversion(make_spec) -> None:
    spec = make_spec()

    class Sneaky:
        @property
        def __class__(self) -> type:
            spec["x-added"] = 1
            return Sneaky

    spec["x-s"] = Sneaky()
    with pytest.raises(RuntimeError, match="dictionary changed size during iteration"):
        osv.validate(spec)


SUBINTERPRETER = """
import os
fd = {fd}
try:
    import openapi_spec_validator_rs as m
    m.validate({{"openapi": "3.0.3", "info": {{"title": "t", "version": "1"}}, "paths": {{}}}})
    result = "ok"
except ImportError as e:
    result = "ImportError: " + str(e)
except BaseException as e:
    result = type(e).__name__ + ": " + str(e)
os.write(fd, result.encode() + b"\\n")
"""


@pytest.mark.skipif(sys.implementation.name != "cpython", reason="CPython subinterpreters")
@pytest.mark.parametrize("main_first", [False, True], ids=["subinterpreter-only", "main-first"])
def test_subinterpreters(main_first: bool) -> None:
    pytest.importorskip("_testcapi")
    script = textwrap.dedent(
        f"""
        import os, _testcapi
        read, write = os.pipe()
        if {main_first}:
            import openapi_spec_validator_rs
        code = {SUBINTERPRETER!r}.format(fd=write)
        _testcapi.run_in_subinterp(code)
        _testcapi.run_in_subinterp(code)
        os.close(write)
        print(os.read(read, 4096).decode(), end="")
        """
    )
    run = subprocess.run([sys.executable, "-c", script], capture_output=True, text=True, timeout=60)
    assert run.returncode == 0, run.stderr
    lines = run.stdout.splitlines()
    if main_first:
        # The extension belongs to the main interpreter: the others are refused cleanly.
        assert all(line.startswith("ImportError: ") for line in lines), lines
    else:
        # The first interpreter to load it may use it; a second one is refused.
        assert lines[0] == "ok"
        assert lines[1].startswith("ImportError: "), lines


def test_keyboard_interrupt_stops_the_work(make_spec) -> None:
    import signal
    import threading
    import time

    if threading.current_thread() is not threading.main_thread():
        pytest.skip("signal handlers run on the main thread only")
    # Thousands of aliases of one big list: seconds of loading.
    items = ", ".join(f"{{a{i}: {i}}}" for i in range(2000))
    text = "openapi: 3.0.3\ninfo: {title: t, version: '1'}\npaths: {}\n"
    text += f"x-base: &b [{items}]\nx-many:\n" + "  - *b\n" * 4000

    def cpu() -> float:
        times = os.times()
        return times.user + times.system

    timer = threading.Timer(0.2, lambda: os.kill(os.getpid(), signal.SIGINT))
    timer.start()
    try:
        with pytest.raises(KeyboardInterrupt):
            osv._core.validate_bytes(text.encode(), "<x>", "", None, False, None)
    finally:
        timer.cancel()
    time.sleep(0.3)
    start = cpu()
    time.sleep(1.0)
    # The interrupted work stopped: the process is idle.
    assert cpu() - start < 0.3


AT_EXIT = """
import os
import openapi_spec_validator_rs as osv
from openapi_spec_validator_rs.readers import read_from_filename

spec = {{"openapi": "3.0.3", "info": {{"title": "t", "version": "1"}}, "paths": {{}}}}
if {used}:
    osv.validate(spec)


class Late:
    def __del__(self, write=os.write, osv=osv, read=read_from_filename, spec=spec):
        for call in (lambda: osv.validate(spec), lambda: read({path!r})):
            try:
                call()
                result = "ok"
            except BaseException as e:
                result = type(e).__name__ + ": " + str(e)
            write(1, result.encode() + b"\\n")


keep = Late()
"""


@pytest.mark.parametrize("used", [False, True], ids=["fresh", "used"])
def test_calls_while_the_interpreter_shuts_down(used: bool, tmp_path: Path) -> None:
    # Upstream fails its lazy settings import there; the extension must neither hang nor
    # panic calling back into a dying interpreter.
    path = tmp_path / "spec.json"
    path.write_text('{"openapi": "3.0.3", "info": {"title": "t", "version": "1"}, "paths": {}}')
    script = AT_EXIT.format(used=used, path=str(path))
    run = subprocess.run([sys.executable, "-c", script], capture_output=True, text=True, timeout=60)
    assert run.returncode == 0, run.stderr
    refused = "ImportError: sys.meta_path is None, Python is likely shutting down"
    assert run.stdout.splitlines() == [refused, refused], run.stdout + run.stderr
