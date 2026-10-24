"""``http(s)://`` ``$ref``s, served by a local HTTP server.

Upstream downloads such documents through jsonschema-path's default handlers; the port
does the same, lazily and once per document, and reports every failure as an
unresolvable reference. When openapi-spec-validator is installed, its output on the same
documents is compared byte for byte.
"""

from __future__ import annotations

import http.server
import json
import os
import threading
import time
from collections.abc import Callable, Iterator
from pathlib import Path
from types import ModuleType
from typing import Any

import openapi_spec_validator_rs as osv
import pytest
from openapi_spec_validator_rs import _core, handlers
from openapi_spec_validator_rs.__main__ import main

# The reference is upstream's default backend, even if jsonschema-rs is installed.
os.environ["OPENAPI_SPEC_VALIDATOR_SCHEMA_VALIDATOR_BACKEND"] = "jsonschema"

Route = tuple[int, bytes, dict[str, str]]

USER = json.dumps({"type": "object", "properties": {"name": {"type": "string"}}}).encode()
BAD_DEFAULT = json.dumps({"type": "integer", "default": "x"}).encode()
INT = json.dumps({"type": "integer"}).encode()
DOC_YAML = (
    b"openapi: 3.0.0\ninfo: {title: R, version: '1'}\npaths: {}\ncomponents:\n"
    b"  schemas:\n    User: {type: object, properties: {id: {type: integer, default: oops}}}\n"
)


class Server:
    """A local HTTP server answering from ``routes`` and recording the requests."""

    def __init__(self) -> None:
        self.routes: dict[str, Route] = {}
        self.hits: list[str] = []
        server = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args: Any) -> None:
                pass

            def do_GET(self) -> None:
                server.hits.append(self.path)
                path = self.path.split("?")[0]
                status, body, headers = server.routes.get(
                    path, (404, b"<html>nope</html>", {"Content-Type": "text/html"})
                )
                if "X-Delay" in headers:
                    time.sleep(float(headers["X-Delay"]))
                self.send_response(status)
                for name, value in headers.items():
                    if not name.startswith("X-"):
                        self.send_header(name, value)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        self._server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.base = f"http://127.0.0.1:{self._server.server_address[1]}"
        threading.Thread(target=self._server.serve_forever, daemon=True).start()

    def url(self, path: str) -> str:
        return self.base + path

    def serve(self, path: str, body: bytes, content_type: str = "application/json", **headers: str) -> str:
        self.routes[path] = (200, body, {"Content-Type": content_type, **headers})
        return self.url(path)

    def close(self) -> None:
        self._server.shutdown()
        self._server.server_close()


@pytest.fixture
def server() -> Iterator[Server]:
    server = Server()
    yield server
    server.close()


def spec(reference: str, version: str = "3.0.0") -> dict[str, Any]:
    """An OpenAPI 3.0 document whose only response schema is ``$ref: reference``."""
    return {
        "openapi": version,
        "info": {"title": "t", "version": "1"},
        "paths": {
            "/u": {
                "get": {
                    "responses": {
                        "200": {
                            "description": "ok",
                            "content": {"application/json": {"schema": {"$ref": reference}}},
                        }
                    }
                }
            }
        },
    }


def lazy_spec(reference: str) -> dict[str, Any]:
    """A document with an invalid local default in ``paths`` and ``$ref: reference`` in
    ``components``, which the walk reaches later."""
    document = spec(reference)
    document["paths"]["/u"]["get"]["parameters"] = [
        {"name": "q", "in": "query", "schema": {"type": "integer", "default": "local"}}
    ]
    document["paths"]["/u"]["get"]["responses"]["200"].pop("content")
    document["components"] = {"schemas": {"R": {"$ref": reference}}}
    return document


def test_remote_documents_are_fetched_and_validated(server: Server) -> None:
    user = server.serve("/user.json", USER)
    bad = server.serve("/bad-default.json", BAD_DEFAULT)
    doc = server.serve("/doc.yaml", DOC_YAML, "text/plain")

    assert osv.validate(spec(user)) is None
    with pytest.raises(osv.OpenAPIValidationError) as info:
        osv.validate(spec(bad))
    assert info.value.message == "'x' is not of type 'integer'"
    with pytest.raises(osv.OpenAPIValidationError) as info:
        osv.validate(spec(doc + "#/components/schemas/User"))
    assert info.value.message == "'oops' is not of type 'integer'"
    assert server.hits == ["/user.json", "/bad-default.json", "/doc.yaml"]


def test_failures_make_the_reference_unresolvable(server: Server, monkeypatch: pytest.MonkeyPatch) -> None:
    # With `requests` installed a space would be quoted and a 304 accepted, as upstream
    # does then (see test_same_as_upstream); these are urllib's rules.
    monkeypatch.setattr(handlers, "_requests", None)
    server.routes["/500"] = (500, b"boom", {})
    server.routes["/304"] = (304, b"", {})
    server.routes["/loop"] = (302, b"", {"Location": "/loop"})
    server.serve("/bad.yaml", b"a: [\n", "text/plain")
    server.serve("/a b.json", USER)
    for reference in [
        server.url("/missing.json"),
        server.url("/500"),
        server.url("/304"),
        server.url("/loop"),
        server.url("/bad.yaml"),
        server.url("/a b.json"),
        "https://127.0.0.1:1/user.json",
        "ftp://127.0.0.1/user.json",
    ]:
        with pytest.raises(osv.ValidationAbortedError) as info:
            osv.validate(spec(reference))
        assert str(info.value) == reference


def test_each_document_is_fetched_once_and_only_when_reached(server: Server) -> None:
    user = server.serve("/user.json", USER)
    bad = server.serve("/bad-default.json", BAD_DEFAULT)
    document = spec(user)
    document["components"] = {
        "schemas": {
            "A": {"$ref": user},
            "B": {"$ref": user},
            # Upstream resolves no references inside examples or extensions.
            "C": {"type": "object", "example": {"$ref": bad}},
        }
    }
    document["info"]["x-ext"] = {"$ref": bad}
    assert osv.validate(document) is None
    assert server.hits == ["/user.json"]

    # A first-error validation stops before reaching the reference.
    lazy = lazy_spec(bad)
    server.hits.clear()
    with pytest.raises(osv.OpenAPIValidationError) as info:
        osv.validate(lazy)
    assert info.value.message == "'local' is not of type 'integer'"
    assert server.hits == []
    errors = list(osv.OpenAPIV30SpecValidator(lazy).iter_errors())
    assert [error.message for error in errors] == [
        "'local' is not of type 'integer'",
        "'x' is not of type 'integer'",
    ]
    assert server.hits == ["/bad-default.json"]


def test_validate_url_resolves_relative_references(server: Server) -> None:
    server.serve("/schemas/address.json", json.dumps(
        {"type": "object", "properties": {"zip": {"type": "integer", "default": "notint"}}}
    ).encode())
    server.serve("/user.json", json.dumps(
        {"type": "object", "properties": {"address": {"$ref": "schemas/address.json"}}}
    ).encode())
    url = server.serve("/openapi.json", json.dumps(spec("user.json")).encode())
    with pytest.raises(osv.OpenAPIValidationError) as info:
        osv.validate_url(url)
    assert info.value.message == "'notint' is not of type 'integer'"
    assert server.hits == ["/openapi.json", "/user.json", "/schemas/address.json"]


def test_command_line(server: Server, write: Callable[[str, str], Path], capsys: Any) -> None:
    user = server.serve("/user.json", USER)
    bad = server.serve("/bad-default.json", BAD_DEFAULT)
    valid = write("valid.yaml", json.dumps(spec(user)))
    invalid = write("invalid.yaml", json.dumps(spec(bad)))
    missing = write("missing.yaml", json.dumps(spec(server.url("/missing.json"))))

    main([str(valid)])
    assert capsys.readouterr().out == f"{valid}: OK\n"
    with pytest.raises(SystemExit) as info:
        main([str(invalid)])
    assert info.value.code == 1
    assert capsys.readouterr().out.startswith(
        f"{invalid}: Validation Error: 'x' is not of type 'integer'\n"
    )
    with pytest.raises(SystemExit) as info:
        main([str(missing)])
    assert info.value.code == 2
    assert capsys.readouterr().out == f"{missing}: Error: {server.url('/missing.json')}\n"


def test_urllib_handler_uses_a_body_cut_short(server: Server, monkeypatch: pytest.MonkeyPatch) -> None:
    class Truncating(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args: Any) -> None:
            pass

        def do_GET(self) -> None:
            self.send_response(200)
            self.send_header("Content-Length", str(len(BAD_DEFAULT) + 1000))
            self.end_headers()
            self.wfile.write(BAD_DEFAULT)
            self.close_connection = True

    truncating = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Truncating)
    threading.Thread(target=truncating.serve_forever, daemon=True).start()
    try:
        monkeypatch.setattr(handlers, "_requests", None)
        url = f"http://127.0.0.1:{truncating.server_address[1]}/truncated.json"
        # PyYAML reads the stream in chunks, so upstream validates what arrived.
        with pytest.raises(osv.OpenAPIValidationError) as info:
            osv.validate(spec(url))
        assert info.value.message == "'x' is not of type 'integer'"
    finally:
        truncating.shutdown()
        truncating.server_close()


def test_handler_timeout(server: Server, monkeypatch: pytest.MonkeyPatch) -> None:
    slow = server.serve("/slow.json", USER, **{"X-Delay": "3"})
    monkeypatch.setattr(handlers, "TIMEOUT", 0.5)
    started = time.monotonic()
    with pytest.raises(osv.ValidationAbortedError) as info:
        osv.validate(spec(slow))
    assert str(info.value) == slow
    assert time.monotonic() - started < 2.5


def test_urllib_handler_returns_the_raw_body(server: Server, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(handlers, "_requests", None)
    url = server.serve("/utf8.yaml", "type: integer\ndefault: café\n".encode(), "text/plain")
    assert handlers.retrieve(url) == "type: integer\ndefault: café\n".encode()
    with pytest.raises(osv.OpenAPIValidationError) as info:
        osv.validate(spec(url))
    assert info.value.message == "'café' is not of type 'integer'"
    latin1 = server.serve("/latin1.yaml", b"type: string\ndescription: caf\xe9\n", "text/plain")
    with pytest.raises(osv.ValidationAbortedError):
        osv.validate(spec(latin1))


def test_requests_handler_uses_the_decoded_text(monkeypatch: pytest.MonkeyPatch) -> None:
    calls: list[tuple[str, int, bool]] = []

    class Response:
        def __init__(self, text: str, status: int) -> None:
            self.text = text
            self.status = status

        def raise_for_status(self) -> None:
            if self.status >= 400:
                raise OSError(f"status {self.status}")

    def get(url: str, timeout: int, verify: bool) -> Response:
        calls.append((url, timeout, verify))
        if url.endswith("/404"):
            return Response("", 404)
        # What requests makes of UTF-8 served as text/plain without a charset.
        return Response("type: integer\ndefault: cafÃ©\n", 200)

    fake = ModuleType("requests")
    fake.get = get  # type: ignore[attr-defined]
    monkeypatch.setattr(handlers, "_requests", fake)

    assert handlers.retrieve("https://example.com/utf8.yaml") == (
        "type: integer\ndefault: cafÃ©\n".encode()
    )
    with pytest.raises(OSError):
        handlers.retrieve("https://example.com/404")
    assert calls == [
        ("https://example.com/utf8.yaml", 10, True),
        ("https://example.com/404", 10, True),
    ]
    with pytest.raises(osv.OpenAPIValidationError) as info:
        osv.validate(spec("https://example.com/utf8.yaml"))
    assert info.value.message == "'cafÃ©' is not of type 'integer'"
    with pytest.raises(osv.ValidationAbortedError):
        osv.validate(spec("https://example.com/404"))


def test_requests_is_used_when_installed(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(handlers, "_requests", handlers._UNRESOLVED)
    try:
        import requests
    except ImportError:
        assert handlers._requests_module() is None
    else:
        assert handlers._requests_module() is requests


@pytest.mark.parametrize("exception", [KeyboardInterrupt, SystemExit])
def test_interrupts_in_the_handler_propagate(
    server: Server,
    write: Callable[[str, str], Path],
    monkeypatch: pytest.MonkeyPatch,
    exception: type[BaseException],
) -> None:
    def interrupted(url: str) -> bytes:
        raise exception(url)

    monkeypatch.setattr(handlers, "retrieve", interrupted)
    reference = server.url("/user.json")
    with pytest.raises(exception):
        osv.validate(spec(reference))
    path = write("remote.json", json.dumps(spec(reference)))
    with pytest.raises(exception):
        main([str(path)])
    assert server.hits == []


def test_exceptions_in_the_handler_are_unresolvable(server: Server, monkeypatch: pytest.MonkeyPatch) -> None:
    def failing(url: str) -> bytes:
        raise RuntimeError(url)

    monkeypatch.setattr(handlers, "retrieve", failing)
    reference = server.url("/user.json")
    with pytest.raises(osv.ValidationAbortedError) as info:
        osv.validate(spec(reference))
    assert str(info.value) == reference
    # A handler returning something other than bytes is an error too.
    monkeypatch.setattr(handlers, "retrieve", lambda url: "text")
    with pytest.raises(osv.ValidationAbortedError):
        osv.validate(spec(reference))


def test_core_without_a_retriever(server: Server) -> None:
    reference = server.serve("/user.json", USER)
    # The public API always passes a retriever; the extension alone does not fetch.
    with pytest.raises(osv.ValidationAbortedError):
        _core.validate(spec(reference), "3.0", "", False)
    assert server.hits == []


# --- byte-for-byte comparison with openapi-spec-validator itself ---------------------

upstream = pytest.importorskip("openapi_spec_validator")
import openapi_spec_validator.__main__ as upstream_main  # noqa: E402


def api_outcome(mod: Any, document: Any, base_uri: str, all_errors: bool) -> Any:
    try:
        cls = mod.shortcuts.get_validator_cls(document)
        validator = cls(document, base_uri=base_uri)
        if all_errors:
            return ("errors", [str(e) for e in validator.iter_errors()])
        validator.validate()
        return ("errors", [])
    except Exception as error:  # noqa: BLE001 - the class is what is compared
        if isinstance(error, (osv.OpenAPIValidationError, upstream.validation.exceptions.OpenAPIValidationError)):
            return ("errors", [str(error)])
        name = type(error).__name__
        if mod is upstream:
            name = "ValidationAbortedError"
        return ("raises", name, str(error))


def cli_outcome(entry: Callable[[list[str]], Any], args: list[str], capsys: Any) -> tuple[int, str, str]:
    code = 0
    try:
        entry(args)
    except SystemExit as exit:
        code = exit.code if isinstance(exit.code, int) else 1
    captured = capsys.readouterr()
    return (code, captured.out, captured.err)


def remote_cases(server: Server) -> dict[str, dict[str, Any]]:
    user = server.serve("/user.json", USER)
    bad = server.serve("/bad-default.json", BAD_DEFAULT)
    doc = server.serve("/doc.yaml", DOC_YAML, "text/plain")
    int_ = server.serve("/int.json", INT)
    server.serve("/nested.json", json.dumps({"type": "object", "properties": {"deep": {"$ref": "user-rel.json"}}}).encode())
    server.serve("/user-rel.json", json.dumps({"type": "object", "properties": {"address": {"$ref": "schemas/address.json"}}}).encode())
    server.serve("/schemas/address.json", json.dumps({"type": "object", "properties": {"zip": {"type": "integer", "default": "notint"}}}).encode())
    server.serve("/bom.json", b"\xef\xbb\xbf" + json.dumps({"type": "integer", "default": "bom"}).encode())
    server.serve("/utf8.yaml", "type: integer\ndefault: café\n".encode(), "text/plain")
    server.serve("/latin1.yaml", b"type: string\ndescription: caf\xe9\n", "text/plain")
    server.serve("/bad.yaml", b"a: [\n", "text/plain")
    server.serve("/param.json", json.dumps({"name": "q", "in": "query", "type": "integer", "default": "badparam"}).encode())
    server.routes["/500"] = (500, b"boom", {})
    server.routes["/304"] = (304, b"", {})
    server.serve("/a b.json", USER)
    server.routes["/redirect"] = (302, b"", {"Location": "/user.json"})
    server.routes["/loop"] = (302, b"", {"Location": "/loop"})

    cases = {
        "valid": spec(user),
        "bad-default": spec(bad),
        "fragment": spec(doc + "#/components/schemas/User"),
        "nested": spec(server.url("/nested.json")),
        "query": spec(bad + "?v=1"),
        "bom": spec(server.url("/bom.json")),
        "utf8-text": spec(server.url("/utf8.yaml")),
        "latin1-text": spec(server.url("/latin1.yaml")),
        "bad-yaml": spec(server.url("/bad.yaml")),
        "404": spec(server.url("/missing.json")),
        "500": spec(server.url("/500")),
        "redirect": spec(server.url("/redirect")),
        "loop": spec(server.url("/loop")),
        "refused": spec("http://127.0.0.1:1/user.json"),
        "https-to-plain": spec(user.replace("http://", "https://")),
        "dot-segments": spec(server.url("/sub/../bad-default.json")),
        "space": spec(server.url("/a b.json")),
        "304": spec(server.url("/304")),
        "userinfo": spec(user.replace("http://", "http://u:p@")),
        "lazy": lazy_spec(bad),
        "swagger-param": {
            "swagger": "2.0",
            "info": {"title": "t", "version": "1"},
            "paths": {"/u": {"get": {
                "parameters": [{"$ref": server.url("/param.json")}],
                "responses": {"200": {"description": "ok"}},
            }}},
        },
        "discriminator": {
            "openapi": "3.0.0",
            "info": {"title": "t", "version": "1"},
            "paths": {},
            "components": {"schemas": {
                "Pet": {
                    "type": "object",
                    "properties": {"kind": {"type": "string"}},
                    "discriminator": {"propertyName": "kind", "mapping": {"dog": int_}},
                    "oneOf": [{"$ref": "#/components/schemas/Dog"}],
                    "default": {"kind": "dog"},
                },
                "Dog": {"type": "object"},
            }},
        },
    }
    for version in ("3.0.0", "3.1.0"):
        cases[f"default-ref-{version[:3]}"] = {
            "openapi": version,
            "info": {"title": "t", "version": "1"},
            "paths": {},
            "components": {"schemas": {"A": {
                "type": "object",
                "properties": {"x": {"$ref": int_}},
                "default": {"x": "str"},
            }}},
        }
    return cases


def test_same_as_upstream(server: Server, write: Callable[[str, str], Path], capsys: Any) -> None:
    for name, document in remote_cases(server).items():
        for all_errors in (False, True):
            expected = api_outcome(upstream, document, "", all_errors)
            actual = api_outcome(osv, document, "", all_errors)
            assert actual == expected, (name, all_errors)
        path = write(f"{name}.json", json.dumps(document))
        for args in ([str(path)], ["--validation-errors", "all", str(path)]):
            expected = cli_outcome(upstream_main.main, args, capsys)
            actual = cli_outcome(main, args, capsys)
            assert actual == expected, (name, args)
