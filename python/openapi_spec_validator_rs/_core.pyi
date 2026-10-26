"""Native module: document conversion, validation and the command line."""

from collections.abc import Callable
from typing import Any, Final, final

__all__ = ["CoreError", "__version__", "cli_main", "load", "validate", "validate_bytes"]

__version__: Final[str]

def _check_interpreter() -> None: ...
def _check_finalizing() -> None: ...

@final
class CoreError:
    """One validation error, as produced by the validator."""

    @property
    def kind(self) -> str: ...
    @property
    def message(self) -> str: ...
    @property
    def has_details(self) -> bool: ...
    @property
    def keyword(self) -> str | None: ...
    @property
    def schema_path(self) -> list[str | int]: ...
    @property
    def instance_path(self) -> list[str | int]: ...
    @property
    def cause(self) -> str | None: ...
    def schema(self) -> Any: ...
    def instance(self) -> Any: ...
    def context(self) -> list[CoreError]: ...
    def render(self) -> str: ...

#: Fetches the document an ``http(s)://`` ``$ref`` points to; any ``Exception`` makes
#: the reference unresolvable, anything else is re-raised by the calling function.
Retriever = Callable[[str], bytes]

def validate(
    spec: object,
    version: str,
    base_uri: str,
    all_errors: bool,
    retriever: Retriever | None = None,
    python_depth: int | None = None,
    recursion_limit: int | None = None,
    follow_root_ref: bool = True,
) -> list[CoreError]: ...
def validate_bytes(
    data: bytes,
    stream_name: str,
    base_uri: str,
    version: str | None,
    all_errors: bool,
    retriever: Retriever | None = None,
    python_depth: int | None = None,
    recursion_limit: int | None = None,
) -> list[CoreError]: ...
def load(data: bytes, stream_name: str) -> Any: ...
def cli_main(
    args: list[str],
    stdin: bytes | None = None,
    retriever: Retriever | None = None,
) -> tuple[int, str, str, bool]: ...
