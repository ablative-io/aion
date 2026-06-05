from __future__ import annotations

import pytest

from aion_client import AlreadyExists, NotFound, QueryTimeout, Unavailable
from aion_client.errors import ServerFailure, map_error


def test_maps_not_found_wire_error() -> None:
    error = map_error(ServerFailure(code="not_found", message="missing"))
    assert isinstance(error, NotFound)


def test_maps_start_idempotency_conflict_to_already_exists() -> None:
    error = map_error(ServerFailure(code="sequence_conflict", message="conflict", operation="start"), operation="start")
    assert isinstance(error, AlreadyExists)


def test_maps_query_timeout_wire_error() -> None:
    error = map_error(ServerFailure(code="query_timeout", message="deadline", operation="query"), operation="query")
    assert isinstance(error, QueryTimeout)


def test_maps_transport_os_error_to_unavailable() -> None:
    error = map_error(OSError("connection refused"))
    assert isinstance(error, Unavailable)


def test_public_exceptions_are_branchable() -> None:
    with pytest.raises(NotFound):
        raise map_error(ServerFailure(code="not_found", message="missing"))
