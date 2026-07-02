"""Protocol session over generated gRPC stubs."""

from __future__ import annotations

import asyncio
import logging
from collections.abc import AsyncIterator, Iterable
from dataclasses import dataclass
from typing import Any, Protocol, TypeAlias, cast

import grpc
from google.protobuf.message import Message

from .proto import common_pb2, worker_pb2, worker_pb2_grpc

Payload: TypeAlias = common_pb2.Payload
logger = logging.getLogger(__name__)

_GRPC_EOF: Any = cast(Any, grpc.aio).EOF
"""grpc.aio's end-of-stream sentinel returned by ``read()``.

Accessed through ``cast`` because grpcio's type stubs do not declare it; the
runtime attribute is stable public API.
"""

WorkflowId: TypeAlias = common_pb2.WorkflowId
ActivityId: TypeAlias = common_pb2.ActivityId
ActivityError: TypeAlias = worker_pb2.ActivityError
ActivityTaskMessage: TypeAlias = worker_pb2.ActivityTask


class WorkerSessionError(Exception):
    """Base error raised by the worker session substrate."""


class RegistrationError(WorkerSessionError):
    """Raised when activity registration is invalid or cannot be sent."""


class TransportError(WorkerSessionError):
    """Raised when the worker stream cannot send or receive frames."""


@dataclass(frozen=True)
class ReconnectConfig:
    """Operator-supplied reconnect policy with no SDK defaults.

    The policy governs both session establishment and the run loop's
    cumulative mid-run session-drop budget of ``max_attempts``.

    Budget reset: the cumulative drop budget resets to zero once an
    established session proves healthy — it served at least one task, or it
    stayed connected longer than ``max_backoff_seconds`` (measured
    monotonically from successful registration to the moment the stream ended
    or dropped; post-drop draining of in-flight handlers never extends it).
    The cap is the policy's own definition of the longest pause, so a session
    outliving it is demonstrably past the flapping regime, and a served task
    proves end-to-end health. A genuinely flapping server — no session ever
    serves a task or outlives ``max_backoff_seconds`` — exhausts the budget
    after exactly ``max_attempts`` drops.

    Drains and clean closes: a server-announced drain (the wire
    ``DrainRequest`` frame) is an unbudgeted drop — the worker finishes
    in-flight work and redials after ``initial_backoff_seconds``; the drain
    classification latches for the session, so even an abrupt end after the
    frame stays drain-class. An *unannounced* clean stream close remains a
    budgeted retryable drop: the worker redials through the same budgeted,
    backed-off cycle, and only a persistent unannounced clean-close loop
    exhausts the budget (raising ``ReconnectError`` from
    ``ServerClosedStreamError``).

    Shutdown during establishment or a backoff: shutdown wins promptly
    during BOTH backoff phases — the session-establishment retries and the
    mid-run drop backoffs — AND during an in-flight establishment attempt.
    Every SDK races each backoff sleep and the whole dial/handshake/register
    chain against the shutdown signal and never dials again once it fires
    (the Rust worker selects shutdown around the entire establishment; this
    SDK cancels the in-flight attempt task, which closes its
    partially-established session while unwinding). The run outcome is
    aligned across the Rust, Python, and TypeScript workers: a pending
    drain-class or clean-close drop ends the run cleanly, while a pending
    error-class drop re-raises its error — a supervisor sees "this worker
    was mid-fault" distinctly from "this worker drained cleanly".
    """

    initial_backoff_seconds: float
    max_backoff_seconds: float
    max_attempts: int


@dataclass(frozen=True)
class TransportCredentials:
    """Opaque credential passthrough for future AW transport metadata."""

    metadata: tuple[tuple[str, str], ...] = ()


@dataclass(frozen=True)
class WorkerConfig:
    """Explicit worker configuration consumed by session, loop, and reconnect."""

    endpoint: str
    task_queue: str
    identity: str
    max_concurrency: int
    reconnect: ReconnectConfig
    transport_credentials: TransportCredentials | None = None
    namespace: str = "default"
    subject: str = "worker"

    def grpc_metadata(self) -> tuple[tuple[str, str], ...]:
        """Return authorization metadata for the worker stream connection.

        WorkerConfig owns the Aion authorization headers. Preserve any unrelated
        transport metadata while dropping reserved auth header duplicates so stale
        credential metadata cannot shadow the configured namespace or subject.
        """
        auth_metadata = (
            ("x-aion-namespaces", self.namespace),
            ("x-aion-subject", self.subject),
        )
        if self.transport_credentials is None:
            return auth_metadata
        reserved_headers = {key for key, _value in auth_metadata}
        transport_metadata = tuple(
            (key, value) for key, value in self.transport_credentials.metadata if key.lower() not in reserved_headers
        )
        return (*transport_metadata, *auth_metadata)


@dataclass(frozen=True)
class ActivityTask:
    """Type-erased activity invocation delivered by the engine."""

    workflow_id: WorkflowId
    activity_id: ActivityId
    activity_type: str
    input: Payload
    attempt: int

    @classmethod
    def from_proto(cls, task: ActivityTaskMessage) -> ActivityTask:
        if task.attempt == 0:
            # proto3 zero default = the producer failed to stamp the attempt.
            raise TransportError("activity task attempt is missing or zero (producer failed to stamp it)")
        return cls(
            workflow_id=task.workflow_id,
            activity_id=task.activity_id,
            activity_type=task.activity_type,
            input=task.input,
            attempt=task.attempt,
        )


@dataclass(frozen=True)
class TaskReceived:
    """Session event containing a new activity task."""

    task: ActivityTask


@dataclass(frozen=True)
class ActivityCancelled:
    """Session event marking cooperative cancellation for an activity."""

    workflow_id: WorkflowId
    activity_id: ActivityId


@dataclass(frozen=True)
class ResultAcknowledged:
    """The server consumed the identified ``ActivityResult`` frame.

    The worker may stop re-reporting it: the serve loop clears the matching
    unacked-tracker entry. An ack with no matching entry is a logged no-op.
    """

    workflow_id: WorkflowId
    activity_id: ActivityId


@dataclass(frozen=True)
class DrainReceived:
    """Server-initiated drain: the server is going away.

    The worker finishes in-flight work, reports what it can, stops expecting
    new tasks, and reconnects after the schedule's initial backoff. The
    classification latches for the session: the eventual stream end — clean
    or abrupt — is drain-class and consumes no drop budget.
    """


WorkerSessionEvent: TypeAlias = TaskReceived | ActivityCancelled | ResultAcknowledged | DrainReceived


@dataclass(frozen=True)
class RegisteredSessionInfo:
    """Server-assigned registration facts carried by the ``RegisterAck``."""

    worker_id: int
    namespace: str
    heartbeat_window_ms: int


class WorkerSession(Protocol):
    """Fake-friendly transport abstraction hiding generated gRPC stubs."""

    async def handshake(self, config: WorkerConfig) -> None:
        """Open the worker stream and advertise task queue plus identity."""

    async def register(self, activity_types: Iterable[str], available_handlers: Iterable[str]) -> None:
        """Register activity type names implemented by this worker."""

    def receive_tasks(self) -> AsyncIterator[WorkerSessionEvent]:
        """Return an async iterator of server-pushed worker events."""

    async def report_result(self, workflow_id: WorkflowId, activity_id: ActivityId, result: Payload) -> None:
        """Report a completed activity result."""

    async def report_failure(self, workflow_id: WorkflowId, activity_id: ActivityId, failure: ActivityError) -> None:
        """Report an explicitly classified activity failure."""

    async def send_heartbeat(
        self,
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        progress: Payload | None,
    ) -> None:
        """Send cooperative activity progress."""


class GrpcWorkerSession:
    """gRPC-backed session using AW's bidirectional WorkerProtocol stream."""

    def __init__(self, config: WorkerConfig, channel: grpc.aio.Channel | None = None) -> None:
        self._config = config
        self._channel = channel
        self._stub: worker_pb2_grpc.WorkerProtocolStub | None = None
        self._outbound: asyncio.Queue[worker_pb2.WorkerToServer | None] | None = None
        self._stream: Any | None = None
        self._activity_types: list[str] = []
        self._registered_info: RegisteredSessionInfo | None = None

    @property
    def registered_info(self) -> RegisteredSessionInfo | None:
        """Server-assigned registration facts, available once registered."""

        return self._registered_info

    def heartbeat_window_seconds(self) -> float | None:
        """Server-assigned liveness window from the ``RegisterAck``, in seconds.

        The serve loop derives its AUTOMATIC liveness-heartbeat cadence from
        this window: the server's heartbeat sweeper expires any worker whose
        in-flight task goes longer than the window without a heartbeat, so the
        runtime — not each handler — keeps every in-flight activity beating.
        ``None`` before registration (and for a degenerate zero window)
        disables the automatic pump.
        """

        info = self._registered_info
        if info is None or info.heartbeat_window_ms <= 0:
            return None
        return info.heartbeat_window_ms / 1000.0

    @classmethod
    async def connect(cls, config: WorkerConfig) -> GrpcWorkerSession:
        logger.info("Connecting to %s", config.endpoint)
        try:
            session = cls(config)
            session._channel = grpc.aio.insecure_channel(config.endpoint)
            session._stub = worker_pb2_grpc.WorkerProtocolStub(session._channel)
            return session
        except Exception as exc:
            logger.error("Connection failed to %s: %s", config.endpoint, exc)
            raise

    async def handshake(self, config: WorkerConfig) -> None:
        self._config = config
        await self._open_stream()

    async def register(self, activity_types: Iterable[str], available_handlers: Iterable[str]) -> None:
        """Send ``RegisterWorker`` and await the server's ``RegisterAck``.

        Registration succeeds only when the ack — the guaranteed first frame
        on the response stream — arrives. The ack wait is bounded by the
        reconnect policy's ``max_backoff_seconds`` (the operator's own
        definition of the longest tolerable pause); a timeout, a non-ack
        first frame, or a stream that ends before the ack is a retryable
        registration failure. Denials fail the RPC with a gRPC error status
        (``PERMISSION_DENIED`` / ``UNAUTHENTICATED``) exactly as before.
        """

        requested = list(activity_types)
        validate_activity_handlers(requested, available_handlers)
        self._activity_types = requested
        await self._send_register(requested)
        await self._await_register_ack()

    def receive_tasks(self) -> AsyncIterator[WorkerSessionEvent]:
        stream = self._stream
        if stream is None:
            raise TransportError("worker receive stream has not been opened")
        return self._receive_from_stream(stream)

    async def report_result(self, workflow_id: WorkflowId, activity_id: ActivityId, result: Payload) -> None:
        await self._send_to_server(
            worker_pb2.WorkerToServer(
                result=worker_pb2.ActivityResult(
                    workflow_id=workflow_id,
                    activity_id=activity_id,
                    result=result,
                )
            )
        )

    async def report_failure(self, workflow_id: WorkflowId, activity_id: ActivityId, failure: ActivityError) -> None:
        await self._send_to_server(
            worker_pb2.WorkerToServer(
                result=worker_pb2.ActivityResult(
                    workflow_id=workflow_id,
                    activity_id=activity_id,
                    error=failure,
                )
            )
        )

    async def send_heartbeat(self, workflow_id: WorkflowId, activity_id: ActivityId, progress: Payload | None) -> None:
        heartbeat = worker_pb2.Heartbeat(workflow_id=workflow_id, activity_id=activity_id)
        if progress is not None:
            heartbeat.progress.CopyFrom(progress)
        await self._send_to_server(worker_pb2.WorkerToServer(heartbeat=heartbeat))

    async def close(self) -> None:
        outbound = self._outbound
        if outbound is not None:
            await outbound.put(None)
        channel = self._channel
        if channel is not None:
            await channel.close()

    async def _open_stream(self) -> None:
        if self._stub is None:
            if self._channel is None:
                self._channel = grpc.aio.insecure_channel(self._config.endpoint)
            self._stub = worker_pb2_grpc.WorkerProtocolStub(self._channel)
        self._outbound = asyncio.Queue(maxsize=16)
        stream_worker = cast(Any, self._stub.StreamWorker)
        self._stream = stream_worker(self._outbound_messages(), metadata=self._config.grpc_metadata())

    async def _outbound_messages(self) -> AsyncIterator[worker_pb2.WorkerToServer]:
        outbound = self._outbound
        if outbound is None:
            raise TransportError("worker outbound stream has not been opened")
        while True:
            message = await outbound.get()
            if message is None:
                return
            yield message

    async def _send_register(self, activity_types: list[str]) -> None:
        register = worker_pb2.RegisterWorker(
            namespace=self._config.task_queue,
            activity_types=activity_types,
        )
        await self._send_to_server(worker_pb2.WorkerToServer(register=register))

    async def _await_register_ack(self) -> None:
        """Read the response stream's first frame and demand ``RegisterAck``."""

        stream = self._stream
        if stream is None:
            raise TransportError("worker receive stream has not been opened")
        deadline = self._config.reconnect.max_backoff_seconds
        try:
            first = await asyncio.wait_for(cast(Any, stream).read(), timeout=deadline)
        except asyncio.TimeoutError as exc:
            raise TransportError(f"server did not acknowledge registration within {deadline}s") from exc
        except grpc.RpcError as exc:
            # Denials (PERMISSION_DENIED / UNAUTHENTICATED) surface here as
            # the RPC's error status; the cause chain preserves the code for
            # the reconnect machinery's retryability classification.
            raise TransportError(f"worker registration failed: {exc}") from exc
        if first is _GRPC_EOF:
            raise TransportError("server ended the stream before acknowledging registration")
        message = cast(worker_pb2.ServerToWorker, first)
        if message.WhichOneof("message") != "register_ack":
            raise TransportError(
                "protocol violation: server sent a non-RegisterAck frame before acknowledging registration"
            )
        ack = message.register_ack
        self._registered_info = RegisteredSessionInfo(
            worker_id=ack.worker_id,
            namespace=ack.namespace,
            heartbeat_window_ms=ack.heartbeat_window_ms,
        )

    async def _send_to_server(self, message: worker_pb2.WorkerToServer) -> None:
        outbound = self._outbound
        if outbound is None:
            raise TransportError("worker stream has not been opened")
        try:
            outbound.put_nowait(message)
        except asyncio.QueueFull as exc:
            raise TransportError("worker outbound queue is full") from exc

    async def _receive_from_stream(
        self,
        stream: Any,
    ) -> AsyncIterator[WorkerSessionEvent]:
        # A read() loop, not `async for`: grpc.aio forbids mixing read() with
        # the async iterator on one call, and register() consumed frame 1
        # (the RegisterAck) via read().
        try:
            while True:
                message = await cast(Any, stream).read()
                if message is _GRPC_EOF:
                    return
                yield decode_server_message(cast(worker_pb2.ServerToWorker, message))
        except grpc.RpcError as exc:
            raise TransportError(f"worker stream receive failed: {exc}") from exc


def validate_activity_handlers(activity_types: Iterable[str], available_handlers: Iterable[str]) -> None:
    available = set(available_handlers)
    for activity_type in activity_types:
        if activity_type not in available:
            raise RegistrationError(f"missing handler for activity type {activity_type!r}")


def decode_server_message(message: Message) -> WorkerSessionEvent:
    """Decode a raw gRPC server-to-worker message into a typed session event."""
    if not isinstance(message, worker_pb2.ServerToWorker):
        raise TransportError("server-to-worker message had unexpected type")
    which = message.WhichOneof("message")
    if which == "task":
        return TaskReceived(ActivityTask.from_proto(message.task))
    if which == "drain":
        return DrainReceived()
    if which == "result_ack":
        ack = message.result_ack
        return ResultAcknowledged(workflow_id=ack.workflow_id, activity_id=ack.activity_id)
    if which == "register_ack":
        # The ack is consumed inside register(); a second one mid-stream is a
        # server ordering bug that must surface.
        raise TransportError("protocol violation: RegisterAck received after registration completed")
    if which is None:
        raise TransportError("server-to-worker message had no payload set")
    # Genuinely unknown oneofs surface what was actually received instead of
    # a misleading "empty message" report.
    raise TransportError(f"unsupported server-to-worker message {which!r}")
