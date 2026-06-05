"""Async event iterator with sequence-number resumption."""

from __future__ import annotations

from collections.abc import AsyncIterator, Awaitable, Callable
from dataclasses import dataclass
import importlib
from inspect import isawaitable
import json
from types import ModuleType
from typing import Any, Generic, TypeAlias, TypeVar, cast

from .errors import AionClientError, InvalidArgument, Unavailable, map_error
from .payload import Payload, decode_payload, payload_from_wire

T = TypeVar("T")
TransportFactory: TypeAlias = Callable[[int | None], AsyncIterator[Any] | Awaitable[AsyncIterator[Any]]]


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
    """Async iterator that reconnects and resumes from the last delivered seq."""

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
    ) -> None:
        self.endpoint = endpoint
        self.namespace = namespace
        self.workflow_id = workflow_id
        self.run_id = run_id
        self.auth = auth
        self.decoder = decoder
        self.raw = raw
        self._transport_factory = transport_factory or self._websocket_transport
        self._last_seq: int | None = None
        self._current: AsyncIterator[Any] | None = None
        self._closed = False

    @property
    def last_sequence(self) -> int | None:
        """The last delivered per-workflow event sequence number."""

        return self._last_seq

    def __aiter__(self) -> "EventStream[T]":
        return self

    async def __anext__(self) -> StreamEvent[T] | T:
        if self._closed:
            raise StopAsyncIteration
        while True:
            if self._current is None:
                self._current = await self._open(self._resume_from())
            try:
                raw_frame = await self._current.__anext__()
            except StopAsyncIteration as exc:
                raise Unavailable("event stream ended before a terminal event") from exc
            except TransientStreamDisconnect:
                self._current = None
                continue
            except TerminalStreamFailure as exc:
                raise Unavailable(str(exc) or "event stream failed terminally") from exc
            except BaseException as exc:
                if isinstance(exc, AionClientError):
                    raise
                mapped = map_error(exc, operation="subscribe")
                if isinstance(mapped, Unavailable):
                    self._current = None
                    continue
                raise mapped from exc

            decoded = self._decode_frame(raw_frame)
            if decoded.seq <= (self._last_seq or 0):
                continue
            self._last_seq = decoded.seq
            if self.raw:
                return cast(StreamEvent[T] | T, decoded)
            return decoded.value

    async def aclose(self) -> None:
        """Close iteration normally at the caller's request."""

        self._closed = True

    def _resume_from(self) -> int | None:
        if self._last_seq is None:
            return None
        return self._last_seq + 1

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
        except ImportError as exc:
            raise InvalidArgument("websockets is required for subscribe") from exc

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

    def _decode_frame(self, frame: Any) -> StreamEvent[T]:
        event = _frame_event(frame)
        payload = _event_payload(event)
        decoded_any = decode_payload(payload, self.decoder)
        seq = _extract_seq(decoded_any)
        return StreamEvent(seq=seq, value=cast(T, decoded_any), raw=event)


def _subscription_request(namespace: str, workflow_id: str, resume_from: int | None) -> dict[str, object]:
    del resume_from
    return {
        "per_workflow": {
            "namespace": namespace,
            "workflow_id": {"uuid": workflow_id},
        }
    }


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
            return Payload(content_type="application/json", bytes=json.dumps(event, separators=(",", ":")).encode("utf-8"))
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
