from __future__ import annotations

from dataclasses import dataclass
from types import SimpleNamespace

import pytest

from aion_client import (
    AlreadyExists,
    InvalidArgument,
    NamespaceDenied,
    NotFound,
    QueryFailed,
    QueryTimeout,
    ServerError,
    Unauthenticated,
    Unavailable,
)
from aion_client.errors import ErrorCode, ServerFailure, map_error, map_query_error


class _FakeRpcError(Exception):
    """Structural stand-in for ``grpc.aio.AioRpcError`` (``code()``/``details()``)."""

    def __init__(self, name: str, details: str) -> None:
        super().__init__(details)
        self._name = name
        self._details = details

    def code(self) -> SimpleNamespace:
        return SimpleNamespace(name=self._name)

    def details(self) -> str:
        return self._details


@dataclass(frozen=True)
class _WireQueryError:
    """Structural stand-in for the AW QueryResponse error payload."""

    code: int | str
    message: str
    detail: str | None = None


def test_maps_not_found_wire_error() -> None:
    error = map_error(ServerFailure(code="not_found", message="missing"))
    assert isinstance(error, NotFound)


def test_maps_start_idempotency_conflict_to_already_exists() -> None:
    error = map_error(ServerFailure(code="already_exists", message="conflict", operation="start"), operation="start")
    assert isinstance(error, AlreadyExists)


def test_sequence_conflict_is_server_fault_even_on_start() -> None:
    error = map_error(ServerFailure(code="sequence_conflict", message="conflict", operation="start"), operation="start")
    assert isinstance(error, ServerError)
    assert not isinstance(error, AlreadyExists)


def test_maps_query_timeout_wire_error() -> None:
    error = map_error(ServerFailure(code="query_timeout", message="deadline", operation="query"), operation="query")
    assert isinstance(error, QueryTimeout)


def test_maps_transport_os_error_to_unavailable() -> None:
    error = map_error(OSError("connection refused"))
    assert isinstance(error, Unavailable)


def test_public_exceptions_are_branchable() -> None:
    with pytest.raises(NotFound):
        raise map_error(ServerFailure(code="not_found", message="missing"))


def test_namespace_denied_code_string() -> None:
    assert ErrorCode.NAMESPACE_DENIED.value == "NamespaceDenied"
    assert NamespaceDenied.code is ErrorCode.NAMESPACE_DENIED


def test_grpc_permission_denied_maps_to_namespace_denied() -> None:
    error = map_error(_FakeRpcError("PERMISSION_DENIED", "namespace 'prod' is not granted"))
    assert isinstance(error, NamespaceDenied)
    assert not isinstance(error, Unauthenticated)
    assert str(error) == "namespace 'prod' is not granted"
    assert error.detail == "namespace 'prod' is not granted"


def test_grpc_unauthenticated_still_maps_to_unauthenticated() -> None:
    error = map_error(_FakeRpcError("UNAUTHENTICATED", "bad token"))
    assert isinstance(error, Unauthenticated)
    assert not isinstance(error, NamespaceDenied)
    assert str(error) == "bad token"


def test_wire_namespace_denied_lowercase_maps_to_namespace_denied() -> None:
    error = map_error(ServerFailure(code="namespace_denied", message="denied", detail="grant missing"))
    assert isinstance(error, NamespaceDenied)
    assert str(error) == "denied"
    assert error.detail == "grant missing"


def test_wire_namespace_denied_screaming_maps_to_namespace_denied() -> None:
    error = map_error(ServerFailure(code="WIRE_ERROR_CODE_NAMESPACE_DENIED", message="denied", detail="grant missing"))
    assert isinstance(error, NamespaceDenied)
    assert str(error) == "denied"
    assert error.detail == "grant missing"


def test_wire_numeric_namespace_denied_maps_to_namespace_denied() -> None:
    error = map_query_error(_WireQueryError(code=2, message="namespace denied", detail="grant missing"))
    assert isinstance(error, NamespaceDenied)
    assert not isinstance(error, QueryFailed)
    assert str(error) == "namespace denied"
    assert error.detail == "grant missing"


def test_map_query_error_never_rewraps_denial_as_query_failed() -> None:
    error = map_query_error(_WireQueryError(code="namespace_denied", message="denied", detail="grant missing"))
    assert isinstance(error, NamespaceDenied)
    assert not isinstance(error, QueryFailed)
    assert str(error) == "denied"
    assert error.detail == "grant missing"


def test_map_query_error_passes_namespace_denied_through_unchanged() -> None:
    original = NamespaceDenied("denied", detail="grant missing")
    assert map_query_error(original) is original


def test_map_error_passes_namespace_denied_through_unchanged() -> None:
    original = NamespaceDenied("denied", detail="grant missing")
    assert map_error(original) is original


def test_wire_invalid_input_lowercase_maps_to_invalid_argument() -> None:
    error = map_error(ServerFailure(code="invalid_input", message="bad input", detail="field x"))
    assert isinstance(error, InvalidArgument)
    assert not isinstance(error, ServerError)
    assert str(error) == "bad input"
    assert error.detail == "field x"


def test_wire_invalid_input_screaming_maps_to_invalid_argument() -> None:
    error = map_error(ServerFailure(code="WIRE_ERROR_CODE_INVALID_INPUT", message="bad input", detail="field x"))
    assert isinstance(error, InvalidArgument)
    assert not isinstance(error, ServerError)
    assert str(error) == "bad input"
    assert error.detail == "field x"


def test_wire_numeric_invalid_input_maps_to_invalid_argument() -> None:
    error = map_query_error(_WireQueryError(code=8, message="bad input", detail="field x"))
    assert isinstance(error, InvalidArgument)
    assert not isinstance(error, ServerError)
    assert str(error) == "bad input"
    assert error.detail == "field x"


def test_wire_numeric_backend_maps_to_server_error() -> None:
    error = map_query_error(_WireQueryError(code=9, message="backend failure", detail="store down"))
    assert isinstance(error, ServerError)
    assert not isinstance(error, InvalidArgument)
    assert str(error) == "backend failure"
    assert error.detail == "store down"


def test_wire_query_failed_lowercase_maps_to_query_failed() -> None:
    error = map_query_error(_WireQueryError(code="query_failed", message="handler raised", detail="boom"))
    assert isinstance(error, QueryFailed)
    assert not isinstance(error, ServerError)
    assert str(error) == "handler raised"
    assert error.detail == "boom"


def test_wire_query_failed_screaming_maps_to_query_failed() -> None:
    error = map_query_error(
        _WireQueryError(code="WIRE_ERROR_CODE_QUERY_FAILED", message="handler raised", detail="boom")
    )
    assert isinstance(error, QueryFailed)
    assert str(error) == "handler raised"
    assert error.detail == "boom"


def test_wire_numeric_query_failed_maps_to_query_failed() -> None:
    # Numeric pin: query_failed is wire enum value 10.
    error = map_query_error(_WireQueryError(code=10, message="handler raised", detail="boom"))
    assert isinstance(error, QueryFailed)
    assert not isinstance(error, ServerError)
    assert str(error) == "handler raised"
    assert error.detail == "boom"


def test_wire_query_failed_maps_to_query_failed_outside_query_operations() -> None:
    # The wire taxonomy is operation-independent: query_failed is QueryFailed
    # wherever it appears, never collapsed to ServerError.
    error = map_error(ServerFailure(code="query_failed", message="handler raised", detail="boom"))
    assert isinstance(error, QueryFailed)
    assert str(error) == "handler raised"
    assert error.detail == "boom"


def test_namespace_denied_is_branchable_and_distinct() -> None:
    with pytest.raises(NamespaceDenied):
        raise map_error(ServerFailure(code="namespace_denied", message="denied"))
    error = map_error(ServerFailure(code="namespace_denied", message="denied"))
    assert not isinstance(error, Unauthenticated)
