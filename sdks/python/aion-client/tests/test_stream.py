from __future__ import annotations

import json
from collections.abc import AsyncIterator

import pytest

from aion_client import NamespaceDenied, Unavailable
from aion_client.stream import (
    EventStream,
    TerminalStreamFailure,
    TransientStreamDisconnect,
    _subscription_request,
)


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
async def test_stream_lagged_before_first_event_reconnects_without_resume_support() -> None:
    """Before any event is delivered no resume cursor is needed, so even a
    transport that cannot resume reconnects on a lagged frame."""

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
        transport_supports_resume=False,
    )

    event = await stream.__anext__()
    assert event == {"seq": 1}
    assert connect_attempts == [None, None]


@pytest.mark.asyncio
async def test_stream_lagged_after_delivery_without_resume_support_raises_unavailable() -> None:
    """A lagged frame after delivered events on a transport that cannot resume
    must surface an honest Unavailable, never InvalidArgument, and must not
    silently reopen a gapped stream."""

    connect_attempts: list[int | None] = []
    lagged_frame = json.dumps(
        {"error": {"code": "lagged", "message": "subscriber lagged behind the broadcast buffer"}},
        separators=(",", ":"),
    )

    async def factory(resume_from: int | None) -> AsyncIterator[object]:
        connect_attempts.append(resume_from)
        return _iter_frames([{"seq": 1}, lagged_frame])

    stream: EventStream[dict[str, int]] = EventStream(
        endpoint="ws://example/events",
        namespace="default",
        workflow_id="wf",
        run_id=None,
        auth=None,
        transport_factory=factory,
        transport_supports_resume=False,
    )

    assert await stream.__anext__() == {"seq": 1}
    with pytest.raises(Unavailable) as excinfo:
        await stream.__anext__()
    assert "resumption" in str(excinfo.value)
    assert connect_attempts == [None]


@pytest.mark.asyncio
async def test_stream_disconnect_after_delivery_without_resume_support_raises_unavailable() -> None:
    """A transient disconnect after delivered events on a transport that
    cannot resume must surface the same honest Unavailable, not reconnect."""

    connect_attempts: list[int | None] = []

    async def factory(resume_from: int | None) -> AsyncIterator[dict[str, int]]:
        connect_attempts.append(resume_from)
        return _iter_events([{"seq": 1}, {"seq": 2}], drop_after_first=True)

    stream: EventStream[dict[str, int]] = EventStream(
        endpoint="ws://example/events",
        namespace="default",
        workflow_id="wf",
        run_id=None,
        auth=None,
        transport_factory=factory,
        transport_supports_resume=False,
    )

    assert await stream.__anext__() == {"seq": 1}
    with pytest.raises(Unavailable) as excinfo:
        await stream.__anext__()
    assert "resumption" in str(excinfo.value)
    assert connect_attempts == [None]


def test_subscription_request_with_resume_cursor_raises_unavailable() -> None:
    """The wire-request guard stays honest: a resume cursor on the websocket
    protocol means an unresumable reopen was attempted upstream."""

    with pytest.raises(Unavailable) as excinfo:
        _subscription_request("default", "wf", 3)
    assert "resumption" in str(excinfo.value)


def test_subscription_request_without_cursor_builds_per_workflow_filter() -> None:
    assert _subscription_request("default", "wf", None) == {
        "per_workflow": {
            "namespace": "default",
            "workflow_id": {"uuid": "wf"},
        }
    }


class _RecordingTransport:
    """Fake transport iterator that records how often aclose is awaited."""

    def __init__(
        self,
        events: list[dict[str, int] | str],
        *,
        aclose_error: Exception | None = None,
        tail_error: Exception | None = None,
    ) -> None:
        self._events = iter(events)
        self._aclose_error = aclose_error
        self._tail_error = tail_error
        self.aclose_calls = 0

    def __aiter__(self) -> _RecordingTransport:
        return self

    async def __anext__(self) -> dict[str, int] | str:
        try:
            return next(self._events)
        except StopIteration:
            if self._tail_error is not None:
                raise self._tail_error from None
            raise StopAsyncIteration from None

    async def aclose(self) -> None:
        self.aclose_calls += 1
        if self._aclose_error is not None:
            raise self._aclose_error


def _stream_over(transport: _RecordingTransport) -> EventStream[dict[str, int]]:
    async def factory(resume_from: int | None) -> AsyncIterator[dict[str, int] | str]:
        return transport

    return EventStream(
        endpoint="ws://example/events",
        namespace="default",
        workflow_id="wf",
        run_id=None,
        auth=None,
        transport_factory=factory,
    )


@pytest.mark.asyncio
async def test_aclose_closes_transport_exactly_once() -> None:
    """aclose must deterministically close the live transport iterator, be
    idempotent on a second call, and end iteration."""

    transport = _RecordingTransport([{"seq": 1}, {"seq": 2}])
    stream = _stream_over(transport)

    assert await stream.__anext__() == {"seq": 1}
    await stream.aclose()
    assert transport.aclose_calls == 1

    await stream.aclose()
    assert transport.aclose_calls == 1

    with pytest.raises(StopAsyncIteration):
        await stream.__anext__()


@pytest.mark.asyncio
async def test_aclose_before_any_iteration_is_safe() -> None:
    """aclose before the transport is ever opened simply ends iteration."""

    transport = _RecordingTransport([{"seq": 1}])
    stream = _stream_over(transport)

    await stream.aclose()
    assert transport.aclose_calls == 0
    with pytest.raises(StopAsyncIteration):
        await stream.__anext__()


@pytest.mark.asyncio
async def test_aclose_after_exhaustion_is_noop() -> None:
    """aclose after the transport exhausted must not raise; the stream is
    still marked closed."""

    transport = _RecordingTransport([{"seq": 1}])
    stream = _stream_over(transport)

    assert await stream.__anext__() == {"seq": 1}
    with pytest.raises(Unavailable):
        await stream.__anext__()

    await stream.aclose()
    assert transport.aclose_calls == 1
    await stream.aclose()
    assert transport.aclose_calls == 1
    with pytest.raises(StopAsyncIteration):
        await stream.__anext__()


@pytest.mark.asyncio
async def test_aclose_transport_failure_propagates_once() -> None:
    """A transport aclose failure is propagated (never swallowed); the stream
    is still closed, so a retried aclose is a no-op and iteration has ended."""

    transport = _RecordingTransport([{"seq": 1}, {"seq": 2}], aclose_error=RuntimeError("close failed"))
    stream = _stream_over(transport)

    assert await stream.__anext__() == {"seq": 1}
    with pytest.raises(RuntimeError, match="close failed"):
        await stream.aclose()
    assert transport.aclose_calls == 1

    await stream.aclose()
    assert transport.aclose_calls == 1
    with pytest.raises(StopAsyncIteration):
        await stream.__anext__()


@pytest.mark.asyncio
async def test_reconnect_after_transient_disconnect_closes_old_transport_once() -> None:
    """A transient-disconnect reconnect must await the dead transport's aclose
    exactly once, before the new transport is opened."""

    first = _RecordingTransport([{"seq": 1}], tail_error=TransientStreamDisconnect("dropped"))
    second = _RecordingTransport([{"seq": 1}, {"seq": 2}])
    # (resume_from, first.aclose_calls observed at open time) per connect.
    opens: list[tuple[int | None, int]] = []

    async def factory(resume_from: int | None) -> AsyncIterator[dict[str, int] | str]:
        opens.append((resume_from, first.aclose_calls))
        return first if len(opens) == 1 else second

    stream: EventStream[dict[str, int]] = EventStream(
        endpoint="ws://example/events",
        namespace="default",
        workflow_id="wf",
        run_id=None,
        auth=None,
        transport_factory=factory,
    )

    assert await stream.__anext__() == {"seq": 1}
    # The redelivered seq-1 event on the second transport is deduped.
    assert await stream.__anext__() == {"seq": 2}
    assert first.aclose_calls == 1
    assert second.aclose_calls == 0
    # Old transport was already closed when the resumed reconnect opened.
    assert opens == [(None, 0), (2, 1)]


@pytest.mark.asyncio
async def test_reconnect_after_lagged_frame_closes_old_transport_once() -> None:
    """A lagged-frame reconnect must likewise close the old transport exactly
    once before opening the new one."""

    lagged_frame = json.dumps(
        {"error": {"code": "lagged", "message": "subscriber lagged behind the broadcast buffer"}},
        separators=(",", ":"),
    )
    first = _RecordingTransport([{"seq": 1}, lagged_frame])
    second = _RecordingTransport([{"seq": 2}])
    opens: list[tuple[int | None, int]] = []

    async def factory(resume_from: int | None) -> AsyncIterator[dict[str, int] | str]:
        opens.append((resume_from, first.aclose_calls))
        return first if len(opens) == 1 else second

    stream: EventStream[dict[str, int]] = EventStream(
        endpoint="ws://example/events",
        namespace="default",
        workflow_id="wf",
        run_id=None,
        auth=None,
        transport_factory=factory,
    )

    assert await stream.__anext__() == {"seq": 1}
    assert await stream.__anext__() == {"seq": 2}
    assert first.aclose_calls == 1
    assert second.aclose_calls == 0
    assert opens == [(None, 0), (2, 1)]


@pytest.mark.asyncio
async def test_reconnect_close_failure_raises_unavailable_with_cause() -> None:
    """If the dead transport's aclose raises during reconnect, the caller gets
    Unavailable with the close error chained as __cause__ and no second
    connect attempt is made."""

    close_error = RuntimeError("close failed")
    transport = _RecordingTransport(
        [{"seq": 1}],
        aclose_error=close_error,
        tail_error=TransientStreamDisconnect("dropped"),
    )
    connect_attempts: list[int | None] = []

    async def factory(resume_from: int | None) -> AsyncIterator[dict[str, int] | str]:
        connect_attempts.append(resume_from)
        return transport

    stream: EventStream[dict[str, int]] = EventStream(
        endpoint="ws://example/events",
        namespace="default",
        workflow_id="wf",
        run_id=None,
        auth=None,
        transport_factory=factory,
    )

    assert await stream.__anext__() == {"seq": 1}
    with pytest.raises(Unavailable) as excinfo:
        await stream.__anext__()
    assert "failed to close during reconnection" in str(excinfo.value)
    assert excinfo.value.__cause__ is close_error
    assert transport.aclose_calls == 1
    assert connect_attempts == [None]


@pytest.mark.asyncio
async def test_stream_first_event_with_seq_zero_is_delivered() -> None:
    """seq 0 is a legitimate first sequence number: it must be delivered, not
    treated as a duplicate, and resume bookkeeping must record it."""

    resume_requests: list[int | None] = []

    async def factory(resume_from: int | None) -> AsyncIterator[dict[str, int]]:
        resume_requests.append(resume_from)
        if resume_from is None:
            return _iter_events([{"seq": 0}, {"seq": 1}], drop_after_first=True)
        return _iter_events([{"seq": 0}, {"seq": 1}])

    stream: EventStream[dict[str, int]] = EventStream(
        endpoint="ws://example/events",
        namespace="default",
        workflow_id="wf",
        run_id=None,
        auth=None,
        transport_factory=factory,
    )

    assert await stream.__anext__() == {"seq": 0}
    assert stream.last_sequence == 0

    # The reconnect resumes from 1 and the redelivered seq-0 event is deduped.
    assert await stream.__anext__() == {"seq": 1}
    assert resume_requests == [None, 1]
    assert stream.last_sequence == 1


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
