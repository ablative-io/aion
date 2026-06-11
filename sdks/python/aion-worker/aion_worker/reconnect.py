"""Backoff reconnect, re-register, and unacknowledged result replay."""

from __future__ import annotations

import asyncio
import inspect
import logging
from collections import OrderedDict
from collections.abc import Awaitable, Callable, Iterable
from dataclasses import dataclass
from typing import Protocol, TypeAlias, cast, runtime_checkable

import grpc

from .session import ActivityError, ActivityId, Payload, WorkerConfig, WorkerSession, WorkflowId

ActivitySequence: TypeAlias = int
PendingReportKey: TypeAlias = tuple[str, int]
"""Tracker key: activity sequence positions are scoped per workflow, so
distinct workflows legitimately collide on the bare position and must be
keyed by workflow as well."""
ConnectFactory: TypeAlias = Callable[[], Awaitable[WorkerSession]]
logger = logging.getLogger(__name__)

SleepFactory: TypeAlias = Callable[[float], Awaitable[None]]

NON_RETRYABLE_STATUS_CODES = frozenset({grpc.StatusCode.PERMISSION_DENIED, grpc.StatusCode.UNAUTHENTICATED})
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


class ServerClosedStreamError(Exception):
    """The server closed the worker stream cleanly without announcing a drain.

    An unannounced clean close is a budgeted retryable session drop: the
    worker redials through the bounded, backed-off reconnect cycle so routine
    server deploys are ridden through. This error is the classified drop
    cause chained as the ``__cause__`` of :class:`ReconnectError` when a
    persistent unannounced clean-close loop exhausts the drop budget. A
    server-ANNOUNCED drain (the wire ``DrainRequest`` frame) is classified
    separately and consumes no budget.
    """


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
    """Tracks reported activity outcomes until the server acks them.

    The server answers every consumed ``ActivityResult`` frame with a
    ``ResultAck``; only that explicit acknowledgement clears an entry — a
    successful send proves nothing on its own. Entries are keyed by
    (workflow uuid, sequence position): activity ids are per-workflow
    sequence positions, so reports from distinct workflows that share a
    position must never replace one another.
    """

    def __init__(self) -> None:
        self._pending: OrderedDict[PendingReportKey, PendingActivityReport] = OrderedDict()

    def record(self, report: PendingActivityReport) -> None:
        self._pending[pending_report_key(report.workflow_id, report.activity_id)] = report

    def acknowledge(self, workflow_id: WorkflowId, activity_id: ActivityId) -> None:
        self._pending.pop(pending_report_key(workflow_id, activity_id), None)

    def get(self, workflow_id: WorkflowId, activity_id: ActivityId) -> PendingActivityReport | None:
        return self._pending.get(pending_report_key(workflow_id, activity_id))

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
    shutdown: asyncio.Event | None,
    sleep: SleepFactory = asyncio.sleep,
) -> WorkerSession | None:
    """Connect, handshake, and register with bounded exponential backoff.

    Matches the Rust reference: the backoff loop wraps connect AND
    handshake/register so that a server that accepts TCP but rejects
    handshakes backs off exponentially rather than hammering at
    initial_backoff_seconds. Deterministic PERMISSION_DENIED / UNAUTHENTICATED
    denials are re-raised immediately instead of consuming further attempts.

    Shutdown wins promptly throughout the establishment cycle exactly as it
    does during the run loop's drop backoff: every backoff sleep AND every
    in-flight establishment attempt (dial, handshake, register) is raced
    against ``shutdown``, and no further dial is attempted once it fires —
    parity with the Rust worker, which selects shutdown around the entire
    establishment in ``run_with_connector_until``, so a SIGTERM during a
    hung dial returns promptly instead of waiting out the transport's own
    connect behaviour. Returns ``None`` when shutdown ended the
    establishment cycle so the caller returns cleanly; a failed attempt's
    partially-established session is always closed before the backoff
    begins, and an attempt cancelled by shutdown closes its
    partially-established session on the way out.
    """

    last_error: BaseException | None = None
    for attempt in range(1, backoff.max_attempts + 1):
        if shutdown is not None and shutdown.is_set():
            logger.info("Shutdown requested during connection establishment; not dialling")
            return None
        try:
            session = await _establish_or_shutdown(connect, config, activity_types, available_handlers, shutdown)
            if session is None:
                return None
            return session
        except Exception as exc:
            logger.error("Connection failed to %s: %s", config.endpoint, exc)
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
            await sleep_or_shutdown(sleep, delay, shutdown)
    raise ReconnectError(f"worker reconnect attempts exhausted for {config.endpoint}: {last_error}") from last_error


async def _establish_session(
    connect: ConnectFactory,
    config: WorkerConfig,
    activity_types: Iterable[str],
    available_handlers: Iterable[str],
) -> WorkerSession:
    """Run one full establishment attempt: dial, handshake, register.

    The partially-established session is closed on the attempt's OWN exit
    path for EVERY non-success — an ordinary failure or a cancellation when
    shutdown wins the race mid-handshake or mid-register — so an abandoned
    attempt never leaks its channel (``BaseException`` deliberately includes
    ``asyncio.CancelledError``; the close runs inside the cancelled task's
    unwinding before the cancellation is re-raised).
    """

    session: WorkerSession | None = None
    try:
        session = await connect()
        await session.handshake(config)
        await session.register(activity_types, available_handlers)
    except BaseException:
        await close_failed_session(session)
        raise
    return session


async def _establish_or_shutdown(
    connect: ConnectFactory,
    config: WorkerConfig,
    activity_types: Iterable[str],
    available_handlers: Iterable[str],
    shutdown: asyncio.Event | None,
) -> WorkerSession | None:
    """Race one full establishment attempt against the shutdown event.

    When shutdown wins, the in-flight attempt task is cancelled and awaited
    to completion: :func:`_establish_session` closes its partially-created
    session while unwinding, so the channel never leaks. An attempt that
    fails on its own just as shutdown wins is logged — acceptable only
    because the worker is exiting — never re-raised and never left as an
    unretrieved task exception. Returns ``None`` when shutdown ended the
    attempt; a completed attempt's success or failure propagates unchanged.
    """

    if shutdown is None:
        return await _establish_session(connect, config, activity_types, available_handlers)
    attempt_task = asyncio.ensure_future(_establish_session(connect, config, activity_types, available_handlers))
    shutdown_task: asyncio.Task[bool] = asyncio.create_task(shutdown.wait())
    try:
        wait_tasks = cast(set[asyncio.Future[object]], {attempt_task, shutdown_task})
        done, _pending = await asyncio.wait(wait_tasks, return_when=asyncio.FIRST_COMPLETED)
        if attempt_task in done:
            return attempt_task.result()
        logger.info("Shutdown requested during connection establishment; abandoning the in-flight dial")
        attempt_task.cancel()
        try:
            await attempt_task
        except asyncio.CancelledError:
            if not attempt_task.cancelled():
                # The CancelledError is the enclosing coroutine's own
                # cancellation, not the attempt acknowledging ours.
                raise
        except Exception as exc:
            logger.warning("worker reconnect attempt abandoned at shutdown failed: %s", exc)
        return None
    finally:
        shutdown_task.cancel()
        attempt_task.cancel()


async def sleep_or_shutdown(sleep: SleepFactory, delay: float, shutdown: asyncio.Event | None) -> None:
    """Run the injected backoff sleep, waking immediately when shutdown fires.

    A worker told to stop during a long backoff — an establishment retry or a
    mid-run drop recovery — must never stall for the remainder of the delay
    (a SIGTERM-to-SIGKILL window in orchestrated deployments). The caller
    re-checks the shutdown event after this returns. A sleep that completes
    first propagates its own failure, matching a directly awaited sleep.
    """

    if shutdown is None:
        await sleep(delay)
        return
    if shutdown.is_set():
        return
    sleep_task = asyncio.ensure_future(sleep(delay))
    shutdown_task: asyncio.Task[bool] = asyncio.create_task(shutdown.wait())
    try:
        wait_tasks = cast(set[asyncio.Future[object]], {sleep_task, shutdown_task})
        done, pending = await asyncio.wait(wait_tasks, return_when=asyncio.FIRST_COMPLETED)
        for pending_task in pending:
            pending_task.cancel()
        if sleep_task in done:
            sleep_task.result()
    finally:
        shutdown_task.cancel()
        sleep_task.cancel()


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


async def re_report_unacked(session: WorkerSession, tracker: UnackedResultTracker) -> None:
    """Re-send every unacknowledged report in deterministic sequence order.

    Server ``ResultAck`` frames clear entries mid-session, so the
    steady-state backlog is empty and this replay decays to the still-unacked
    residue. Entries are not removed by sending; only an explicit ack clears
    the tracker.
    """

    for report in tracker.snapshot():
        if isinstance(report, PendingCompletedReport):
            await session.report_result(report.workflow_id, report.activity_id, report.output)
        else:
            await session.report_failure(report.workflow_id, report.activity_id, report.failure)


def activity_sequence(activity_id: ActivityId) -> ActivitySequence:
    """Extract the deterministic sequence position from an activity identifier."""
    return int(activity_id.sequence_position)


def pending_report_key(workflow_id: WorkflowId, activity_id: ActivityId) -> PendingReportKey:
    """Build the per-workflow tracker key for an unacknowledged report."""
    return (str(workflow_id.uuid), activity_sequence(activity_id))
