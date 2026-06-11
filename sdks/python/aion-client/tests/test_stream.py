from __future__ import annotations

import json
import sys
from collections.abc import AsyncIterator
from types import ModuleType, TracebackType
from typing import Any

import pytest

from aion_client import InvalidArgument, NamespaceDenied, Unavailable
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


def test_subscription_request_with_resume_cursor_emits_wire_field() -> None:
    """A resume cursor rides inside per_workflow as resume_from_seq, the
    first sequence number wanted."""

    assert _subscription_request("default", "wf", 3) == {
        "per_workflow": {
            "namespace": "default",
            "workflow_id": {"uuid": "wf"},
            "resume_from_seq": 3,
        }
    }


def test_subscription_request_rejects_cursor_below_one() -> None:
    """resume_from_seq = 0 is invalid_input on the wire; the request builder
    refuses to ever send it (the resume loop sends last_delivered + 1 >= 1)."""

    with pytest.raises(InvalidArgument) as excinfo:
        _subscription_request("default", "wf", 0)
    assert "resume_from_seq" in str(excinfo.value)


def test_subscription_request_without_cursor_omits_resume_field() -> None:
    """The first connect carries no cursor: the resume_from_seq key is absent
    entirely (never present as 0 or null)."""

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
async def test_clean_transport_end_ends_iteration_and_closes_once() -> None:
    """A graceful server close (WebSocket close-1000) ends the transport
    iterator cleanly; the stream ends iteration normally — never a synthetic
    Unavailable — closes the exhausted transport deterministically, and a
    later aclose is a no-op."""

    transport = _RecordingTransport([{"seq": 1}])
    stream = _stream_over(transport)

    assert await stream.__anext__() == {"seq": 1}
    with pytest.raises(StopAsyncIteration):
        await stream.__anext__()
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


class _FakeWebSocket:
    """Scripted server side of one websocket connection.

    Yields the scripted frames in order; an optional ``tail_error`` is raised
    after the last frame (standing in for an abnormal closure mid-stream).
    """

    def __init__(self, frames: list[str], *, tail_error: BaseException | None = None) -> None:
        self._frames = iter(frames)
        self._tail_error = tail_error
        self.sent: list[dict[str, Any]] = []

    async def send(self, message: str) -> None:
        decoded = json.loads(message)
        assert isinstance(decoded, dict)
        self.sent.append(decoded)

    def __aiter__(self) -> _FakeWebSocket:
        return self

    async def __anext__(self) -> str:
        try:
            return next(self._frames)
        except StopIteration:
            if self._tail_error is not None:
                raise self._tail_error from None
            raise StopAsyncIteration from None


class _FakeConnection:
    """Async context manager mirroring ``websockets.connect(...)``."""

    def __init__(self, socket: _FakeWebSocket) -> None:
        self._socket = socket

    async def __aenter__(self) -> _FakeWebSocket:
        return self._socket

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: TracebackType | None,
    ) -> bool:
        return False


def _install_fake_websockets(
    monkeypatch: pytest.MonkeyPatch,
    sockets: list[_FakeWebSocket],
    connect_headers: list[dict[str, str]],
) -> type[Exception]:
    """Install a scripted ``websockets`` module pair into ``sys.modules``.

    Returns the fake ``ConnectionClosedError`` class so tests can script
    abnormal closures with the exact type the transport catches.
    """

    class _FakeConnectionClosedError(Exception):
        """Stands in for websockets.exceptions.ConnectionClosedError."""

    pending = iter(sockets)

    def _connect(endpoint: str, additional_headers: dict[str, str] | None = None) -> _FakeConnection:
        assert endpoint == "ws://example/events"
        connect_headers.append(dict(additional_headers or {}))
        try:
            return _FakeConnection(next(pending))
        except StopIteration:
            raise AssertionError("more websocket connections opened than scripted") from None

    module = ModuleType("websockets")
    exceptions_module = ModuleType("websockets.exceptions")
    module.__dict__["connect"] = _connect
    exceptions_module.__dict__["ConnectionClosedError"] = _FakeConnectionClosedError
    monkeypatch.setitem(sys.modules, "websockets", module)
    monkeypatch.setitem(sys.modules, "websockets.exceptions", exceptions_module)
    return _FakeConnectionClosedError


def _wire_frame(seq: int) -> str:
    return json.dumps({"seq": seq}, separators=(",", ":"))


@pytest.mark.asyncio
async def test_builtin_transport_resumes_with_cursor_after_abnormal_closure(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The built-in websocket transport is resume-capable: the first connect
    carries no cursor, an abnormal closure reconnects with resume_from_seq =
    last delivered + 1, the auth header is re-sent, and the replayed overlap
    is deduped so delivery is gap-free and duplicate-free."""

    connect_headers: list[dict[str, str]] = []
    sockets: list[_FakeWebSocket] = []
    closed_error = _install_fake_websockets(monkeypatch, sockets, connect_headers)
    first = _FakeWebSocket(
        [_wire_frame(1), _wire_frame(2)],
        tail_error=closed_error("no close frame received or sent"),
    )
    # A cursor-honoring server would replay exactly from seq 3; replaying 2 as
    # well exercises the dedupe defence-in-depth on top of the cursor.
    second = _FakeWebSocket([_wire_frame(2), _wire_frame(3), _wire_frame(4)])
    sockets.extend([first, second])

    stream: EventStream[dict[str, int]] = EventStream(
        endpoint="ws://example/events",
        namespace="default",
        workflow_id="wf",
        run_id=None,
        auth="token",
    )

    delivered = [await stream.__anext__() for _ in range(4)]
    assert delivered == [{"seq": 1}, {"seq": 2}, {"seq": 3}, {"seq": 4}]
    assert first.sent == [{"per_workflow": {"namespace": "default", "workflow_id": {"uuid": "wf"}}}]
    assert second.sent == [
        {"per_workflow": {"namespace": "default", "workflow_id": {"uuid": "wf"}, "resume_from_seq": 3}}
    ]
    assert connect_headers == [{"authorization": "Bearer token"}, {"authorization": "Bearer token"}]


@pytest.mark.asyncio
async def test_builtin_transport_lagged_frame_reconnects_with_cursor(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A terminal lagged error frame on the built-in transport triggers a
    reconnect that carries the resume cursor instead of raising."""

    lagged_frame = json.dumps(
        {"error": {"code": "lagged", "message": "subscriber lagged behind the broadcast buffer"}},
        separators=(",", ":"),
    )
    connect_headers: list[dict[str, str]] = []
    sockets: list[_FakeWebSocket] = []
    _install_fake_websockets(monkeypatch, sockets, connect_headers)
    first = _FakeWebSocket([_wire_frame(1), lagged_frame])
    second = _FakeWebSocket([_wire_frame(2)])
    sockets.extend([first, second])

    stream: EventStream[dict[str, int]] = EventStream(
        endpoint="ws://example/events",
        namespace="default",
        workflow_id="wf",
        run_id=None,
        auth=None,
    )

    assert await stream.__anext__() == {"seq": 1}
    assert await stream.__anext__() == {"seq": 2}
    assert second.sent == [
        {"per_workflow": {"namespace": "default", "workflow_id": {"uuid": "wf"}, "resume_from_seq": 2}}
    ]


@pytest.mark.asyncio
async def test_resume_round_trip_against_cursor_honoring_transport() -> None:
    """Full resume round-trip: deliver 1..3, drop, the fake honours the
    cursor by replaying exactly from 4 — delivery is exactly 1..6, once each,
    and the reconnect asked for resume_from = last_seq + 1 = 4."""

    all_events = [{"seq": seq} for seq in range(1, 7)]
    resume_requests: list[int | None] = []

    async def _deliver_then_drop(events: list[dict[str, int]]) -> AsyncIterator[dict[str, int]]:
        for event in events:
            yield event
        raise TransientStreamDisconnect("dropped")

    async def factory(resume_from: int | None) -> AsyncIterator[dict[str, int]]:
        resume_requests.append(resume_from)
        if resume_from is None:
            return _deliver_then_drop(all_events[:3])
        # Cursor-honoring: replay exactly the suffix starting at the cursor.
        return _iter_events(all_events[resume_from - 1 :])

    stream: EventStream[dict[str, int]] = EventStream(
        endpoint="ws://example/events",
        namespace="default",
        workflow_id="wf",
        run_id=None,
        auth=None,
        transport_factory=factory,
    )

    delivered = [await stream.__anext__() for _ in range(6)]
    assert delivered == all_events
    assert resume_requests == [None, 4]
    seqs = [event["seq"] for event in delivered if isinstance(event, dict)]
    assert len(seqs) == 6
    assert len(seqs) == len(set(seqs)), "duplicate delivery"
    assert seqs == sorted(seqs) and seqs == list(range(seqs[0], seqs[0] + len(seqs))), "gapped delivery"
    assert stream.last_sequence == 6


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


@pytest.mark.asyncio
async def test_from_seq_carries_the_cursor_on_the_initial_attach() -> None:
    """An explicit starting cursor (from_seq=1 replays full history) rides the
    FIRST attach, and reconnects continue from last delivered + 1."""

    resume_requests: list[int | None] = []

    async def factory(resume_from: int | None) -> AsyncIterator[dict[str, int]]:
        resume_requests.append(resume_from)
        return _iter_events([{"seq": 1}, {"seq": 2}])

    stream: EventStream[dict[str, int]] = EventStream(
        endpoint="ws://example/events",
        namespace="default",
        workflow_id="wf",
        run_id=None,
        auth=None,
        from_seq=1,
        transport_factory=factory,
    )

    assert await stream.__anext__() == {"seq": 1}
    assert await stream.__anext__() == {"seq": 2}
    assert resume_requests == [1], "the initial attach must carry the explicit cursor"


def test_from_seq_below_one_is_rejected() -> None:
    with pytest.raises(InvalidArgument, match="from_seq"):
        EventStream(
            endpoint="ws://example/events",
            namespace="default",
            workflow_id="wf",
            run_id=None,
            auth=None,
            from_seq=0,
        )
