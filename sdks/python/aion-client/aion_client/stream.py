"""Async event iterator with sequence-number resumption."""

from __future__ import annotations

import importlib
import json
from collections.abc import AsyncIterator, Awaitable, Callable
from dataclasses import dataclass
from inspect import isawaitable
from types import ModuleType
from typing import Any, Generic, TypeAlias, TypeVar, cast

from .errors import AionClientError, InvalidArgument, Unavailable, map_error
from .payload import Payload, decode_payload, payload_from_wire

T = TypeVar("T")
TransportFactory: TypeAlias = Callable[[int | None], AsyncIterator[Any] | Awaitable[AsyncIterator[Any]]]

_RESUME_UNSUPPORTED_MESSAGE = (
    "stream disconnected after delivering events and the configured transport "
    "does not support resumption; restart the subscription"
)


@dataclass(frozen=True, slots=True)
class StreamEvent(Generic[T]):
    """Decoded event plus its per-workflow sequence number."""

    seq: int
    value: T
    raw: Any


class TransientStreamDisconnect(Exception):
    """Internal/stub signal that the WebSocket disconnected transiently."""


class TerminalStreamFailure(Exception):
    """Internal/stub signal that the subscription failed terminally."""


class EventStream(Generic[T]):
    """Async iterator that reconnects and resumes from the last delivered seq.

    On reconnect the cursor ``resume_from_seq`` (the first sequence number
    wanted, ``last_delivered + 1``) rides inside the per-workflow subscription
    request; the server replays the missed suffix and splices into the live
    stream, so delivery is gap-free and duplicate-free. Resumption requires a
    transport that can honour the cursor. When an injected transport disclaims
    that capability (``transport_supports_resume=False``), a disconnect after
    at least one delivered event raises :class:`Unavailable` rather than
    silently reopening a gapped stream.
    """

    def __init__(
        self,
        *,
        endpoint: str,
        namespace: str,
        workflow_id: str,
        run_id: str | None,
        auth: str | None,
        decoder: type[T] | None = None,
        raw: bool = False,
        transport_factory: TransportFactory | None = None,
        transport_supports_resume: bool | None = None,
    ) -> None:
        self.endpoint = endpoint
        self.namespace = namespace
        self.workflow_id = workflow_id
        self.run_id = run_id
        self.auth = auth
        self.decoder = decoder
        self.raw = raw
        self._transport_factory = transport_factory or self._websocket_transport
        # Whether the transport can honour a non-None resume_from cursor on
        # reconnect. The built-in websocket transport can: the per-workflow
        # subscription request carries resume_from_seq (the first sequence
        # number wanted), so a reconnect after delivered events replays the
        # missed suffix gap-free. Injected factories receive resume_from per
        # the TransportFactory contract and are presumed to honour it unless
        # explicitly declared otherwise; a factory that disclaims resume gets
        # an honest Unavailable after delivered events instead of a silently
        # gapped reopen.
        if transport_supports_resume is None:
            transport_supports_resume = True
        self._transport_supports_resume = transport_supports_resume
        self._last_seq: int | None = None
        self._current: AsyncIterator[Any] | None = None
        self._closed = False

    @property
    def last_sequence(self) -> int | None:
        """The last delivered per-workflow event sequence number."""

        return self._last_seq

    def __aiter__(self) -> EventStream[T]:
        return self

    async def __anext__(self) -> StreamEvent[T] | T:
        if self._closed:
            raise StopAsyncIteration
        while True:
            if self._current is None:
                resume_from = self._resume_from()
                if resume_from is not None and not self._transport_supports_resume:
                    # A retried __anext__ after the honest Unavailable below:
                    # reopening would silently gap the stream, so refuse again.
                    raise Unavailable(_RESUME_UNSUPPORTED_MESSAGE)
                self._current = await self._open(resume_from)
            try:
                raw_frame = await self._current.__anext__()
            except StopAsyncIteration as exc:
                raise Unavailable("event stream ended before a terminal event") from exc
            except TransientStreamDisconnect as exc:
                await self._prepare_reconnect(exc)
                continue
            except TerminalStreamFailure as exc:
                raise Unavailable(str(exc) or "event stream failed terminally") from exc
            except BaseException as exc:
                if isinstance(exc, AionClientError):
                    raise
                mapped = map_error(exc, operation="subscribe")
                if isinstance(mapped, Unavailable):
                    await self._prepare_reconnect(exc)
                    continue
                raise mapped from exc

            try:
                decoded = self._decode_frame(raw_frame)
            except Unavailable as exc:
                # A retryable server error frame (e.g. "lagged"): reconnect and
                # resume rather than surfacing a transient condition.
                await self._prepare_reconnect(exc)
                continue
            if self._last_seq is not None and decoded.seq <= self._last_seq:
                continue
            self._last_seq = decoded.seq
            if self.raw:
                return cast(StreamEvent[T] | T, decoded)
            return decoded.value

    async def aclose(self) -> None:
        """Close iteration normally at the caller's request.

        Deterministically closes the live transport iterator instead of
        leaving it to garbage-collector finalization. Idempotent: only the
        first call tears down the transport, and closing after exhaustion is
        a no-op. A failure raised by the transport's own ``aclose`` is
        propagated to the caller (never swallowed); the stream is still
        marked closed first, so a retried ``aclose`` does not re-raise and
        iteration cannot resume on a half-closed transport.
        """

        if self._closed:
            return
        self._closed = True
        current, self._current = self._current, None
        if current is None:
            return
        closer = getattr(current, "aclose", None)
        if closer is not None:
            await closer()

    def _resume_from(self) -> int | None:
        if self._last_seq is None:
            return None
        return self._last_seq + 1

    async def _prepare_reconnect(self, cause: BaseException) -> None:
        """Tear down the current transport ahead of a reconnect attempt.

        The dead transport's own ``aclose`` (when it exposes one) is awaited
        before reconnecting, mirroring :meth:`aclose`: non-generator
        transports must not be abandoned to garbage-collector finalization. A
        failure from that close is never swallowed — it surfaces as
        :class:`Unavailable` (retryable in principle) with the close error
        chained via ``__cause__``, and no reconnect is attempted.

        When events have already been delivered the reconnect must resume from
        the last sequence number, otherwise the reopened stream would silently
        gap (a cursor-less reopen does not replay history). If the transport
        cannot honour a resume cursor, raise an honest :class:`Unavailable`
        instead of reconnecting; when the close also failed, this
        resume-needed error wins, with the close error as its ``__cause__``.
        """

        current, self._current = self._current, None
        close_error: Exception | None = None
        if current is not None:
            closer = getattr(current, "aclose", None)
            if closer is not None:
                try:
                    await closer()
                except Exception as exc:
                    close_error = exc
        if self._resume_from() is not None and not self._transport_supports_resume:
            raise Unavailable(_RESUME_UNSUPPORTED_MESSAGE) from (cause if close_error is None else close_error)
        if close_error is not None:
            raise Unavailable(
                f"transport failed to close during reconnection: {close_error}"
            ) from close_error

    async def _open(self, resume_from: int | None) -> AsyncIterator[Any]:
        try:
            candidate = self._transport_factory(resume_from)
            if isawaitable(candidate):
                return await candidate
            return candidate
        except BaseException as exc:
            if isinstance(exc, AionClientError):
                raise
            raise map_error(exc, operation="subscribe") from exc

    async def _websocket_transport(self, resume_from: int | None) -> AsyncIterator[Any]:
        try:
            websockets: ModuleType = importlib.import_module("websockets")
            ws_exceptions: ModuleType = importlib.import_module("websockets.exceptions")
        except ImportError as exc:
            raise InvalidArgument("websockets is required for subscribe") from exc
        # An abnormal closure (no close frame, e.g. a dropped TCP connection)
        # raises ConnectionClosedError from iteration; it is a transient
        # disconnect, so the resume loop reconnects with the cursor. A clean
        # close simply ends iteration.
        connection_closed_error = cast("type[BaseException]", ws_exceptions.ConnectionClosedError)

        headers: dict[str, str] = {}
        if self.auth is not None:
            headers["authorization"] = f"Bearer {self.auth.removeprefix('Bearer ')}"
        request = _subscription_request(self.namespace, self.workflow_id, resume_from)
        try:
            async with websockets.connect(self.endpoint, additional_headers=headers) as websocket:
                await websocket.send(json.dumps(request, separators=(",", ":")))
                async for message in websocket:
                    yield message
        except OSError as exc:
            raise TransientStreamDisconnect(str(exc)) from exc
        except connection_closed_error as exc:
            raise TransientStreamDisconnect(str(exc)) from exc

    def _decode_frame(self, frame: Any) -> StreamEvent[T]:
        event = _frame_event(frame)
        payload = _event_payload(event)
        decoded_any = decode_payload(payload, self.decoder)
        seq = _extract_seq(decoded_any)
        return StreamEvent(seq=seq, value=cast(T, decoded_any), raw=event)


def _subscription_request(namespace: str, workflow_id: str, resume_from: int | None) -> dict[str, object]:
    per_workflow: dict[str, object] = {
        "namespace": namespace,
        "workflow_id": {"uuid": workflow_id},
    }
    if resume_from is not None:
        if resume_from < 1:
            # The cursor is the FIRST sequence number wanted; the server
            # rejects 0 as invalid_input, so it must never reach the wire.
            # EventStream sends last_delivered + 1, which is always >= 1.
            raise InvalidArgument("resume_from_seq must be >= 1 (the first sequence number wanted)")
        per_workflow["resume_from_seq"] = resume_from
    return {"per_workflow": per_workflow}


def _frame_event(frame: Any) -> Any:
    if isinstance(frame, str):
        try:
            frame = json.loads(frame)
        except json.JSONDecodeError as exc:
            raise InvalidArgument(f"event frame is not valid JSON: {exc}") from exc
    if isinstance(frame, bytes):
        try:
            frame = json.loads(frame.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise InvalidArgument(f"event frame is not valid JSON: {exc}") from exc
    if isinstance(frame, dict) and "error" in frame:
        raise map_error(cast(dict[str, object], frame["error"]), operation="subscribe")
    event = getattr(frame, "event", None)
    if event is not None:
        return event
    if isinstance(frame, dict) and "event" in frame:
        return frame["event"]
    return frame


def _event_payload(event: Any) -> Payload:
    if isinstance(event, Payload):
        return event
    if isinstance(event, dict):
        payload = event.get("payload")
        if isinstance(payload, dict):
            content_type = payload.get("content_type")
            raw = payload.get("bytes")
            if isinstance(raw, str):
                raw = raw.encode("utf-8")
            if isinstance(content_type, str) and isinstance(raw, bytes):
                return Payload(content_type=content_type, bytes=raw)
        if "seq" in event:
            encoded = json.dumps(event, separators=(",", ":")).encode("utf-8")
            return Payload(content_type="application/json", bytes=encoded)
    payload = getattr(event, "payload", None)
    if payload is not None:
        return payload_from_wire(payload)
    raise InvalidArgument("stream event does not contain an AW payload")


def _extract_seq(value: Any) -> int:
    if isinstance(value, dict):
        raw_seq = value.get("seq", value.get("sequence", value.get("sequence_number")))
    else:
        raw_seq = getattr(value, "seq", None)
        if raw_seq is None:
            raw_seq = getattr(value, "sequence", None)
        if raw_seq is None:
            raw_seq = getattr(value, "sequence_number", None)
    if isinstance(raw_seq, int) and raw_seq >= 0:
        return raw_seq
    raise InvalidArgument("stream event is missing non-negative seq")
