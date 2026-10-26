"""Native module: document conversion, validation and the command line."""

from collections.abc import Callable
from typing import Any, Final, Generic, TypeVar, final

_T = TypeVar("_T")

__all__ = ["CoreError", "Task", "__version__", "cli_main", "load", "validate", "validate_bytes"]

__version__: Final[str]

def _check_interpreter() -> None: ...
def _check_finalizing() -> None: ...
def _meta_schema(version: str) -> str: ...

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

@final
class Task(Generic[_T]):
    """Work running in the extension, finished by ``_stack.run``: ``step()`` passes on
    the last download's ``body`` (or the ``error`` it raised) and returns the URL to
    download next, or ``None`` once ``result()`` is ready."""

    def step(self, body: object = None, error: BaseException | None = None) -> str | None: ...
    def result(self) -> _T: ...
    def close(self) -> None: ...

#: Fetches the document an ``http(s)://`` ``$ref`` points to when no thread could be
#: started (the task's caller downloads otherwise); any ``Exception`` makes the reference
#: unresolvable, anything else is re-raised by the calling function.
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
) -> Task[list[CoreError]]: ...
def validate_bytes(
    data: bytes,
    stream_name: str,
    base_uri: str,
    version: str | None,
    all_errors: bool,
    retriever: Retriever | None = None,
    python_depth: int | None = None,
    recursion_limit: int | None = None,
) -> Task[list[CoreError]]: ...
def load(data: bytes, stream_name: str) -> Any: ...
def cli_main(
    args: list[str],
    stdin: bytes | None = None,
    retriever: Retriever | None = None,
) -> Task[tuple[int, str, str, bool]]: ...
