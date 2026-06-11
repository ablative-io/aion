from __future__ import annotations

import json
from collections.abc import AsyncIterator

import pytest

from aion_client import NamespaceDenied, Unavailable
from aion_client.stream import EventStream, TerminalStreamFailure, TransientStreamDisconnect


class _PermissionDeniedRpcError(Exception):
    """Structural stand-in for a gRPC PERMISSION_DENIED transport failure."""

    def code(self) -> str:
        return "StatusCode.PERMISSION_DENIED"

    def details(self) -> str:
        return "namespace 'prod' is not granted"


async def _iter_events(
    events: list[dict[str, int]], *, drop_after_first: bool = False
) -> AsyncIterator[dict[str, int]]:
    for index, event in enumerate(events):
        if drop_after_first and index == 1:
            raise TransientStreamDisconnect("dropped")
        yield event


@pytest.mark.asyncio
async def test_stream_resumes_and_skips_duplicates() -> None:
    resume_requests: list[int | None] = []

    async def factory(resume_from: int | None) -> AsyncIterator[dict[str, int]]:
        resume_requests.append(resume_from)
        if resume_from is None:
            return _iter_events([{"seq": 1}, {"seq": 2}], drop_after_first=True)
        return _iter_events([{"seq": 1}, {"seq": 2}, {"seq": 3}])

    stream: EventStream[dict[str, int]] = EventStream(
        endpoint="ws://example/events",
        namespace="default",
        workflow_id="wf",
        run_id="run",
        auth=None,
        transport_factory=factory,
    )

    seen = [await stream.__anext__(), await stream.__anext__(), await stream.__anext__()]
    assert seen == [{"seq": 1}, {"seq": 2}, {"seq": 3}]
    assert resume_requests == [None, 2]


@pytest.mark.asyncio
async def test_stream_terminal_failure_raises() -> None:
    async def broken(_: int | None) -> AsyncIterator[dict[str, int]]:
        raise TerminalStreamFailure("terminal")
        yield {"seq": 1}

    stream: EventStream[dict[str, int]] = EventStream(
        endpoint="ws://example/events",
        namespace="default",
        workflow_id="wf",
        run_id=None,
        auth=None,
        transport_factory=broken,
    )

    with pytest.raises(Unavailable):
        await stream.__anext__()


async def _iter_frames(frames: list[object]) -> AsyncIterator[object]:
    for frame in frames:
        yield frame


@pytest.mark.asyncio
async def test_stream_wrapped_namespace_denied_error_frame_is_terminal() -> None:
    """A `{"error": <WireError>}` frame with snake_case code maps terminally."""

    connect_attempts: list[int | None] = []
    frame = json.dumps(
        {"error": {"code": "namespace_denied", "message": "namespace 'prod' is not granted"}},
        separators=(",", ":"),
    )

    async def factory(resume_from: int | None) -> AsyncIterator[object]:
        connect_attempts.append(resume_from)
        return _iter_frames([frame])

    stream: EventStream[dict[str, int]] = EventStream(
        endpoint="ws://example/events",
        namespace="prod",
        workflow_id="wf",
        run_id=None,
        auth=None,
        transport_factory=factory,
    )

    with pytest.raises(NamespaceDenied) as excinfo:
        await stream.__anext__()
    assert str(excinfo.value) == "namespace 'prod' is not granted"
    assert connect_attempts == [None]


@pytest.mark.asyncio
async def test_stream_wrapped_lagged_error_frame_reconnects() -> None:
    """A `{"error": <WireError>}` lagged frame is retryable: reconnect, no raise."""

    connect_attempts: list[int | None] = []
    lagged_frame = json.dumps(
        {"error": {"code": "lagged", "message": "subscriber lagged behind the broadcast buffer"}},
        separators=(",", ":"),
    )

    async def factory(resume_from: int | None) -> AsyncIterator[object]:
        connect_attempts.append(resume_from)
        if len(connect_attempts) == 1:
            return _iter_frames([lagged_frame])
        return _iter_frames([{"seq": 1}])

    stream: EventStream[dict[str, int]] = EventStream(
        endpoint="ws://example/events",
        namespace="default",
        workflow_id="wf",
        run_id=None,
        auth=None,
        transport_factory=factory,
    )

    event = await stream.__anext__()
    assert event == {"seq": 1}
    assert connect_attempts == [None, None]


@pytest.mark.asyncio
async def test_stream_namespace_denied_is_terminal_not_retried() -> None:
    """A namespace denial must raise immediately, never reconnect as transient."""

    connect_attempts: list[int | None] = []

    async def denied(resume_from: int | None) -> AsyncIterator[dict[str, int]]:
        connect_attempts.append(resume_from)
        raise _PermissionDeniedRpcError()
        yield {"seq": 1}

    stream: EventStream[dict[str, int]] = EventStream(
        endpoint="ws://example/events",
        namespace="prod",
        workflow_id="wf",
        run_id=None,
        auth=None,
        transport_factory=denied,
    )

    with pytest.raises(NamespaceDenied):
        await stream.__anext__()
    assert connect_attempts == [None]
