"""Branchable exception taxonomy for the Aion Python client."""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from enum import Enum
from typing import Any, NoReturn


class ErrorCode(str, Enum):
    """Shared language-neutral client error names."""

    NOT_FOUND = "NotFound"
    ALREADY_EXISTS = "AlreadyExists"
    QUERY_FAILED = "QueryFailed"
    QUERY_TIMEOUT = "QueryTimeout"
    CANCELLED = "Cancelled"
    UNAVAILABLE = "Unavailable"
    UNAUTHENTICATED = "Unauthenticated"
    NAMESPACE_DENIED = "NamespaceDenied"
    INVALID_ARGUMENT = "InvalidArgument"
    SERVER = "Server"


class AionClientError(Exception):
    """Base class for all errors raised by :mod:`aion_client`."""

    code: ErrorCode = ErrorCode.SERVER

    def __init__(self, message: str | None = None, *, detail: str | None = None) -> None:
        self.detail = detail
        super().__init__(message or detail or self.code.value)


class NotFound(AionClientError):
    """The targeted workflow, run, subscription, or entity was not found."""

    code = ErrorCode.NOT_FOUND


class AlreadyExists(AionClientError):
    """A start idempotency key was reused for a conflicting request."""

    code = ErrorCode.ALREADY_EXISTS


class QueryFailed(AionClientError):
    """The workflow query handler ran and returned an application failure."""

    code = ErrorCode.QUERY_FAILED


class QueryTimeout(AionClientError):
    """The query deadline elapsed before a result was available."""

    code = ErrorCode.QUERY_TIMEOUT


class Cancelled(AionClientError):
    """The requested client/server operation was cancelled."""

    code = ErrorCode.CANCELLED


class Unavailable(AionClientError):
    """The server or event stream is temporarily unreachable."""

    code = ErrorCode.UNAVAILABLE


class Unauthenticated(AionClientError):
    """The server rejected or could not validate the caller credential."""

    code = ErrorCode.UNAUTHENTICATED


class NamespaceDenied(AionClientError):
    """The caller has no grant for the requested namespace.

    Raised when the caller's credential is valid but carries no grant for
    the requested namespace. A workflow that does not exist or is owned by
    another namespace surfaces as :class:`NotFound` instead — the response
    is identical in both cases, so existence is never leaked across
    namespaces. Distinct from :class:`Unauthenticated` (credential rejected)
    and not retryable until grants change.
    """

    code = ErrorCode.NAMESPACE_DENIED


class InvalidArgument(AionClientError):
    """The request is syntactically or semantically invalid."""

    code = ErrorCode.INVALID_ARGUMENT


class ServerError(AionClientError):
    """An unexpected server-side failure with preserved diagnostic detail."""

    code = ErrorCode.SERVER


_ERROR_CLASSES: dict[ErrorCode, type[AionClientError]] = {
    ErrorCode.NOT_FOUND: NotFound,
    ErrorCode.ALREADY_EXISTS: AlreadyExists,
    ErrorCode.QUERY_FAILED: QueryFailed,
    ErrorCode.QUERY_TIMEOUT: QueryTimeout,
    ErrorCode.CANCELLED: Cancelled,
    ErrorCode.UNAVAILABLE: Unavailable,
    ErrorCode.UNAUTHENTICATED: Unauthenticated,
    ErrorCode.NAMESPACE_DENIED: NamespaceDenied,
    ErrorCode.INVALID_ARGUMENT: InvalidArgument,
    ErrorCode.SERVER: ServerError,
}

_GRPC_CODE_MAP: dict[str, ErrorCode] = {
    "NOT_FOUND": ErrorCode.NOT_FOUND,
    "ALREADY_EXISTS": ErrorCode.ALREADY_EXISTS,
    "DEADLINE_EXCEEDED": ErrorCode.QUERY_TIMEOUT,
    "CANCELLED": ErrorCode.CANCELLED,
    "UNAVAILABLE": ErrorCode.UNAVAILABLE,
    "UNAUTHENTICATED": ErrorCode.UNAUTHENTICATED,
    "INVALID_ARGUMENT": ErrorCode.INVALID_ARGUMENT,
    "FAILED_PRECONDITION": ErrorCode.INVALID_ARGUMENT,
    "PERMISSION_DENIED": ErrorCode.NAMESPACE_DENIED,
    "RESOURCE_EXHAUSTED": ErrorCode.UNAVAILABLE,
    "INTERNAL": ErrorCode.SERVER,
    "UNKNOWN": ErrorCode.SERVER,
}

_WIRE_CODE_MAP: dict[str, ErrorCode] = {
    "not_found": ErrorCode.NOT_FOUND,
    "WIRE_ERROR_CODE_NOT_FOUND": ErrorCode.NOT_FOUND,
    "namespace_denied": ErrorCode.NAMESPACE_DENIED,
    "WIRE_ERROR_CODE_NAMESPACE_DENIED": ErrorCode.NAMESPACE_DENIED,
    "unknown_query": ErrorCode.INVALID_ARGUMENT,
    "WIRE_ERROR_CODE_UNKNOWN_QUERY": ErrorCode.INVALID_ARGUMENT,
    "query_timeout": ErrorCode.QUERY_TIMEOUT,
    "WIRE_ERROR_CODE_QUERY_TIMEOUT": ErrorCode.QUERY_TIMEOUT,
    "not_running": ErrorCode.INVALID_ARGUMENT,
    "WIRE_ERROR_CODE_NOT_RUNNING": ErrorCode.INVALID_ARGUMENT,
    "lagged": ErrorCode.UNAVAILABLE,
    "WIRE_ERROR_CODE_LAGGED": ErrorCode.UNAVAILABLE,
    # sequence_conflict signals an engine-internal double-writer bug, never an
    # idempotency conflict; it is a server fault on every operation.
    "sequence_conflict": ErrorCode.SERVER,
    "WIRE_ERROR_CODE_SEQUENCE_CONFLICT": ErrorCode.SERVER,
    "invalid_input": ErrorCode.INVALID_ARGUMENT,
    "WIRE_ERROR_CODE_INVALID_INPUT": ErrorCode.INVALID_ARGUMENT,
    "backend": ErrorCode.SERVER,
    "WIRE_ERROR_CODE_BACKEND": ErrorCode.SERVER,
}

_START_IDEMPOTENCY_CODES = {
    "already_exists",
    "ALREADY_EXISTS",
}


@dataclass(frozen=True)
class ServerFailure:
    """A normalized server failure value for transports that do not use gRPC."""

    code: str
    message: str
    detail: str | None = None
    operation: str | None = None


def error_from_code(code: ErrorCode, message: str | None = None, *, detail: str | None = None) -> AionClientError:
    """Build the branchable exception for a normalized taxonomy code."""

    return _ERROR_CLASSES[code](message, detail=detail)


def raise_mapped(error: BaseException, *, operation: str | None = None) -> NoReturn:
    """Raise ``map_error(error)``; useful in ``except`` blocks."""

    raise map_error(error, operation=operation) from error


def map_error(
    error: BaseException | ServerFailure | Mapping[str, object], *, operation: str | None = None
) -> AionClientError:
    """Map transport/server failures to the shared Aion client taxonomy.

    Existing :class:`AionClientError` instances are returned unchanged. gRPC
    ``AioRpcError``-style values are detected structurally by a ``code()``
    method, while simple ``ServerFailure`` and mapping values support test and
    non-gRPC transport adapters.
    """

    if isinstance(error, AionClientError):
        return error
    if isinstance(error, TimeoutError):
        if operation == "query":
            return QueryTimeout(str(error) or "query timed out")
        return Unavailable(str(error) or "operation timed out")
    if isinstance(error, OSError):
        return Unavailable(str(error) or "transport unavailable")
    if isinstance(error, ServerFailure):
        return _map_server_failure(error)
    if isinstance(error, Mapping):
        return _map_mapping(error, operation=operation)

    code_attr = getattr(error, "code", None)
    if callable(code_attr):
        grpc_code = code_attr()
        name = getattr(grpc_code, "name", str(grpc_code).split(".")[-1])
        details_attr = getattr(error, "details", None)
        details = details_attr() if callable(details_attr) else str(error)
        return _map_grpc_status(str(name), str(details), operation=operation)

    return ServerError(str(error) or "unexpected client error", detail=str(error) or None)


def map_query_error(error: Any) -> AionClientError:
    """Map a QueryResponse error payload to ``QueryFailed`` or a specific status."""

    if isinstance(error, AionClientError):
        return error
    mapped = _map_wire_like(error, operation="query")
    if isinstance(
        mapped,
        InvalidArgument | QueryTimeout | NotFound | Unauthenticated | NamespaceDenied | Unavailable | Cancelled,
    ):
        return mapped
    if isinstance(mapped, ServerError):
        return mapped
    return QueryFailed(str(mapped) or "query failed")


def _map_mapping(mapping: Mapping[str, object], *, operation: str | None) -> AionClientError:
    code = mapping.get("code") or mapping.get("status") or mapping.get("error_code")
    message = mapping.get("message") or mapping.get("detail")
    return _map_server_failure(
        ServerFailure(
            code=str(code or "backend"),
            message=str(message or code or "server error"),
            detail=str(mapping.get("detail")) if mapping.get("detail") is not None else None,
            operation=operation,
        )
    )


def _map_server_failure(failure: ServerFailure) -> AionClientError:
    return _map_wire_like(failure, operation=failure.operation)


def _map_wire_like(error: Any, *, operation: str | None) -> AionClientError:
    raw_code = getattr(error, "code", None)
    message = str(getattr(error, "message", "") or getattr(error, "detail", "") or raw_code or "server error")
    detail = getattr(error, "detail", None)

    code_name = _wire_code_name(raw_code)
    if operation == "start" and code_name in _START_IDEMPOTENCY_CODES:
        return AlreadyExists(message, detail=detail)
    if operation == "query" and code_name in {"query_failed", "QUERY_FAILED"}:
        return QueryFailed(message, detail=detail)
    mapped = _WIRE_CODE_MAP.get(code_name)
    if mapped is None:
        mapped = _WIRE_CODE_MAP.get(code_name.lower())
    if mapped is None:
        mapped = ErrorCode.SERVER
    return error_from_code(mapped, message, detail=detail)


def _wire_code_name(raw_code: Any) -> str:
    if raw_code is None:
        return "backend"
    name = getattr(raw_code, "name", None)
    if isinstance(name, str):
        return name
    if isinstance(raw_code, int):
        numeric = {
            1: "WIRE_ERROR_CODE_NOT_FOUND",
            2: "WIRE_ERROR_CODE_NAMESPACE_DENIED",
            3: "WIRE_ERROR_CODE_SEQUENCE_CONFLICT",
            4: "WIRE_ERROR_CODE_UNKNOWN_QUERY",
            5: "WIRE_ERROR_CODE_QUERY_TIMEOUT",
            6: "WIRE_ERROR_CODE_NOT_RUNNING",
            7: "WIRE_ERROR_CODE_LAGGED",
            8: "WIRE_ERROR_CODE_INVALID_INPUT",
            9: "WIRE_ERROR_CODE_BACKEND",
        }
        return numeric.get(raw_code, "WIRE_ERROR_CODE_BACKEND")
    return str(raw_code)


def _map_grpc_status(name: str, details: str, *, operation: str | None) -> AionClientError:
    if operation == "start" and name == "ALREADY_EXISTS":
        return AlreadyExists(details)
    if operation == "query" and name == "DEADLINE_EXCEEDED":
        return QueryTimeout(details)
    return error_from_code(_GRPC_CODE_MAP.get(name, ErrorCode.SERVER), details, detail=details)
