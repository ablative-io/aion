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
    survived longer than ``max_backoff_seconds`` (measured monotonically from
    successful registration to the drop). The cap is the policy's own
    definition of the longest pause, so a session outliving it is
    demonstrably past the flapping regime, and a served task proves
    end-to-end health. A genuinely flapping server — no session ever serves a
    task or outlives ``max_backoff_seconds`` — exhausts the budget after
    exactly ``max_attempts`` drops.

    Clean closes: a clean/graceful server-side stream close is a retryable
    drop, not a run end. The worker redials through the same budgeted,
    backed-off cycle, so routine server deploys cost at most transient budget
    that heals; only a persistent clean-close loop exhausts the budget
    (raising ``ReconnectError`` from ``ServerClosedStreamError``). An
    explicit protocol drain signal ("closing, do not reconnect") is planned
    for the worker-protocol ack wave and will refine the clean-close case.
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

    @classmethod
    def from_proto(cls, task: ActivityTaskMessage) -> ActivityTask:
        return cls(
            workflow_id=task.workflow_id,
            activity_id=task.activity_id,
            activity_type=task.activity_type,
            input=task.input,
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


WorkerSessionEvent: TypeAlias = TaskReceived | ActivityCancelled


class ConnectableStream(Protocol):
    """Subset of gRPC stream call methods used to force connection readiness."""

    async def wait_for_connection(self) -> None:
        """Wait until the underlying RPC is connected or fails."""


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
        self._stream: AsyncIterator[worker_pb2.ServerToWorker] | None = None
        self._activity_types: list[str] = []

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
        requested = list(activity_types)
        validate_activity_handlers(requested, available_handlers)
        self._activity_types = requested
        await self._send_register(requested)
        await self._wait_for_connection()

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

    async def _wait_for_connection(self) -> None:
        stream = self._stream
        if stream is None:
            raise TransportError("worker receive stream has not been opened")
        try:
            await cast(ConnectableStream, stream).wait_for_connection()
        except Exception as exc:
            raise TransportError(f"worker stream connection failed: {exc}") from exc

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
        stream: AsyncIterator[worker_pb2.ServerToWorker],
    ) -> AsyncIterator[WorkerSessionEvent]:
        try:
            async for message in stream:
                yield decode_server_message(message)
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
    raise TransportError("server-to-worker message was empty")
