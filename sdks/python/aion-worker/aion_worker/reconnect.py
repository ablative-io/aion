"""Backoff reconnect, re-register, and unacknowledged result replay."""

from __future__ import annotations

import asyncio
import inspect
import logging
from collections import OrderedDict
from collections.abc import Awaitable, Callable, Iterable
from dataclasses import dataclass
from typing import Protocol, TypeAlias, runtime_checkable

import grpc

from .session import ActivityError, ActivityId, Payload, WorkerConfig, WorkerSession, WorkflowId

ActivitySequence: TypeAlias = int
ConnectFactory: TypeAlias = Callable[[], Awaitable[WorkerSession]]
logger = logging.getLogger(__name__)

SleepFactory: TypeAlias = Callable[[float], Awaitable[None]]

NON_RETRYABLE_STATUS_CODES = frozenset(
    {grpc.StatusCode.PERMISSION_DENIED, grpc.StatusCode.UNAUTHENTICATED}
)
"""Deterministic server denials that no reconnect attempt can ever fix."""


def grpc_status_code(error: BaseException) -> grpc.StatusCode | None:
    """Return the gRPC status code carried by an exception or its cause chain.

    Session errors raised by :mod:`aion_worker.session` wrap the underlying
    ``grpc.aio.AioRpcError`` with ``raise ... from``, so the original status is
    recovered by walking explicit causes rather than matching message strings.
    """

    seen: set[int] = set()
    current: BaseException | None = error
    while current is not None and id(current) not in seen:
        seen.add(id(current))
        code = _direct_status_code(current)
        if code is not None:
            return code
        current = current.__cause__
    return None


def is_retryable_session_error(error: BaseException) -> bool:
    """Return False for PERMISSION_DENIED / UNAUTHENTICATED server denials.

    Those statuses are deterministic (ungranted namespace, rejected
    credentials): retrying only burns the reconnect budget and delays the
    surfaced error. Everything else keeps the bounded backoff behaviour.
    """

    return grpc_status_code(error) not in NON_RETRYABLE_STATUS_CODES


def _direct_status_code(error: BaseException) -> grpc.StatusCode | None:
    if isinstance(error, grpc.aio.AioRpcError):
        return error.code()
    if isinstance(error, grpc.RpcError) and isinstance(error, grpc.Call):
        return error.code()
    return None


@runtime_checkable
class ClosableSession(Protocol):
    """Optional close capability implemented by concrete worker sessions."""

    def close(self) -> object:
        """Close session resources; may return an awaitable."""


class ReconnectError(Exception):
    """Raised when reconnect policy is invalid or exhausted."""


@dataclass(frozen=True)
class PendingCompletedReport:
    """Completion report computed locally but not acknowledged by the engine."""

    workflow_id: WorkflowId
    activity_id: ActivityId
    output: Payload


@dataclass(frozen=True)
class PendingFailedReport:
    """Failure report computed locally but not acknowledged by the engine."""

    workflow_id: WorkflowId
    activity_id: ActivityId
    failure: ActivityError


PendingActivityReport: TypeAlias = PendingCompletedReport | PendingFailedReport


class UnackedResultTracker:
    """Tracks reported activity outcomes until AW adds an explicit ack frame."""

    def __init__(self) -> None:
        self._pending: OrderedDict[ActivitySequence, PendingActivityReport] = OrderedDict()

    def record(self, report: PendingActivityReport) -> None:
        self._pending[activity_sequence(report.activity_id)] = report

    def acknowledge(self, activity_id: ActivityId) -> None:
        self._pending.pop(activity_sequence(activity_id), None)

    def get(self, activity_id: ActivityId) -> PendingActivityReport | None:
        return self._pending.get(activity_sequence(activity_id))

    def snapshot(self) -> tuple[PendingActivityReport, ...]:
        return tuple(self._pending.values())

    def __len__(self) -> int:
        return len(self._pending)

    def is_empty(self) -> bool:
        return not self._pending


@dataclass(frozen=True)
class ReconnectBackoff:
    """Validated bounded exponential reconnect policy."""

    initial_backoff_seconds: float
    max_backoff_seconds: float
    max_attempts: int

    @classmethod
    def from_config(cls, config: WorkerConfig) -> ReconnectBackoff:
        reconnect = config.reconnect
        policy = cls(
            initial_backoff_seconds=reconnect.initial_backoff_seconds,
            max_backoff_seconds=reconnect.max_backoff_seconds,
            max_attempts=reconnect.max_attempts,
        )
        policy.validate()
        return policy

    def validate(self) -> None:
        if self.initial_backoff_seconds <= 0:
            raise ReconnectError("initial reconnect backoff must be greater than zero")
        if self.max_backoff_seconds <= 0:
            raise ReconnectError("max reconnect backoff must be greater than zero")
        if self.max_attempts <= 0:
            raise ReconnectError("max reconnect attempts must be greater than zero")

    def delay_for_attempt(self, attempt: int) -> float:
        if attempt <= 0:
            raise ReconnectError("reconnect attempt must be greater than zero")
        delay = self.initial_backoff_seconds * float(2 ** (attempt - 1))
        return min(delay, self.max_backoff_seconds)


async def reconnect_with_backoff(
    connect: ConnectFactory,
    config: WorkerConfig,
    activity_types: Iterable[str],
    available_handlers: Iterable[str],
    backoff: ReconnectBackoff,
    sleep: SleepFactory = asyncio.sleep,
) -> WorkerSession:
    """Connect, handshake, and register with bounded exponential backoff.

    Matches the Rust reference: the backoff loop wraps connect AND
    handshake/register so that a server that accepts TCP but rejects
    handshakes backs off exponentially rather than hammering at
    initial_backoff_seconds. Deterministic PERMISSION_DENIED / UNAUTHENTICATED
    denials are re-raised immediately instead of consuming further attempts.
    """

    last_error: BaseException | None = None
    for attempt in range(1, backoff.max_attempts + 1):
        session: WorkerSession | None = None
        try:
            session = await connect()
            await session.handshake(config)
            await session.register(activity_types, available_handlers)
            return session
        except Exception as exc:
            logger.error("Connection failed to %s: %s", config.endpoint, exc)
            await close_failed_session(session)
            if not is_retryable_session_error(exc):
                logger.error(
                    "Worker was denied by the server (%s); not retrying",
                    grpc_status_code(exc),
                )
                raise
            last_error = exc
            if attempt == backoff.max_attempts:
                break
            delay = backoff.delay_for_attempt(attempt)
            logger.warning(
                "Reconnecting in %ss (attempt %s/%s)",
                delay,
                attempt,
                backoff.max_attempts,
            )
            await sleep(delay)
    raise ReconnectError(
        f"worker reconnect attempts exhausted for {config.endpoint}: {last_error}"
    ) from last_error


async def close_failed_session(session: WorkerSession | None) -> None:
    """Close a session that failed before entering the serving loop."""

    if session is None or not isinstance(session, ClosableSession):
        return
    try:
        result = session.close()
        if inspect.isawaitable(result):
            await result
    except Exception as exc:
        logger.warning("failed to close unsuccessful worker session: %s", exc)


async def reconnect_register_and_replay(
    connect: ConnectFactory,
    config: WorkerConfig,
    activity_types: Iterable[str],
    available_handlers: Iterable[str],
    tracker: UnackedResultTracker,
    sleep: SleepFactory = asyncio.sleep,
) -> WorkerSession:
    """Reconnect, re-register, then re-report backlog before serving tasks."""

    backoff = ReconnectBackoff.from_config(config)
    session = await reconnect_with_backoff(
        connect, config, activity_types, available_handlers, backoff, sleep,
    )
    await re_report_unacked(session, tracker)
    return session


async def re_report_unacked(session: WorkerSession, tracker: UnackedResultTracker) -> None:
    """Re-send every unacknowledged report in deterministic sequence order."""

    for report in tracker.snapshot():
        if isinstance(report, PendingCompletedReport):
            await session.report_result(report.workflow_id, report.activity_id, report.output)
        else:
            await session.report_failure(report.workflow_id, report.activity_id, report.failure)


def activity_sequence(activity_id: ActivityId) -> ActivitySequence:
    """Extract the deterministic sequence position from an activity identifier."""
    return int(activity_id.sequence_position)
