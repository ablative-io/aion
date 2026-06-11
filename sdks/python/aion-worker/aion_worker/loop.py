"""Asyncio receive, dispatch, and report loop with bounded concurrency."""

from __future__ import annotations

import asyncio
import inspect
import logging
import time
from collections.abc import AsyncIterator, Awaitable, Callable, Iterable
from dataclasses import dataclass
from typing import Protocol, TypeAlias, cast

from .context import WIRE_DEFAULT_ATTEMPT, ActivityCancellationHandle, ActivityContext
from .reconnect import (
    PendingCompletedReport,
    PendingFailedReport,
    ReconnectBackoff,
    ReconnectError,
    ServerClosedStreamError,
    UnackedResultTracker,
    grpc_status_code,
    is_retryable_session_error,
)
from .session import (
    ActivityCancelled,
    ActivityError,
    ActivityId,
    ActivityTask,
    Payload,
    TaskReceived,
    WorkerConfig,
    WorkerSession,
    WorkerSessionEvent,
    WorkflowId,
)

logger = logging.getLogger(__name__)
SessionFactory: TypeAlias = Callable[[], Awaitable[WorkerSession]]
SleepFactory: TypeAlias = Callable[[float], Awaitable[None]]
ActivityExecutionContext: TypeAlias = ActivityContext
_InFlightKey: TypeAlias = tuple[str, int]


class ShutdownRequested:
    """Sentinel returned when the serve loop observes shutdown."""


class StreamFinished:
    """Sentinel returned when the server ends the receive stream cleanly.

    The reconnect-aware run loop treats this as a retryable session drop —
    never as a run end — so workers ride through graceful server closes.
    """


@dataclass
class SessionHealth:
    """Per-session liveness counters used for drop-budget reset accounting."""

    tasks_served: int = 0


@dataclass(frozen=True)
class Completed:
    """Dispatch outcome carrying successful output payload."""

    output: Payload


@dataclass(frozen=True)
class Failed:
    """Dispatch outcome carrying explicit classified activity failure."""

    failure: ActivityError


DispatchOutcome: TypeAlias = Completed | Failed


class ActivityDispatcher(Protocol):
    """Typed-activity seam filled by AR-008."""

    def activity_types(self) -> Iterable[str]:
        """Return activity type names this dispatcher can serve."""

    async def dispatch(self, task: ActivityTask, context: ActivityExecutionContext) -> DispatchOutcome:
        """Run one activity and return a completion or explicit failure."""


async def serve(
    config: WorkerConfig,
    session: WorkerSession,
    dispatcher: ActivityDispatcher,
    tracker: UnackedResultTracker | None = None,
    shutdown: asyncio.Event | None = None,
    health: SessionHealth | None = None,
) -> ShutdownRequested | StreamFinished:
    """Serve tasks from an already connected and registered session.

    Returns :class:`ShutdownRequested` when the caller's shutdown event ended
    the loop, or :class:`StreamFinished` when the server ended the task
    stream cleanly; the reconnect-aware caller treats the latter as a
    retryable session drop. When ``health`` is supplied, every task whose
    outcome report was sent on this session increments
    ``health.tasks_served`` — the end-to-end proof used for drop-budget
    resets, surviving even when a later failure ends the session.
    """

    if config.max_concurrency <= 0:
        raise ValueError("max_concurrency must be greater than zero")
    unacked = tracker if tracker is not None else UnackedResultTracker()
    semaphore = asyncio.Semaphore(config.max_concurrency)
    running: set[asyncio.Task[None]] = set()
    in_flight: dict[_InFlightKey, ActivityCancellationHandle] = {}
    stream = session.receive_tasks().__aiter__()

    try:
        while True:
            if shutdown is not None and shutdown.is_set():
                return ShutdownRequested()
            event = await _receive_next_or_shutdown(stream, shutdown)
            if isinstance(event, ShutdownRequested | StreamFinished):
                return event
            if isinstance(event, TaskReceived):
                await semaphore.acquire()
                task = asyncio.create_task(
                    _run_and_report(session, dispatcher, event.task, unacked, semaphore, in_flight, health)
                )
                running.add(task)
                task.add_done_callback(running.discard)
            else:
                _handle_control_event(event, in_flight)
    finally:
        if shutdown is not None and shutdown.is_set():
            for handle in in_flight.values():
                handle.cancel()
        if running:
            await asyncio.gather(*running)


async def connect_register_replay_and_serve(
    config: WorkerConfig,
    connect: SessionFactory,
    dispatcher: ActivityDispatcher,
    tracker: UnackedResultTracker | None = None,
    sleep: SleepFactory = asyncio.sleep,
    shutdown: asyncio.Event | None = None,
    clock: Callable[[], float] = time.monotonic,
) -> None:
    """Connect, register, replay unacked reports, then serve with drop recovery.

    The run ends only on graceful shutdown, a non-retryable server denial, or
    exhaustion of the cumulative session-drop budget
    (``reconnect.max_attempts``). A clean/graceful server stream close is a
    retryable drop: the worker redials through the same budgeted, backed-off
    cycle, so routine server deploys are ridden through. The budget resets to
    zero once an established session proves healthy — it served at least one
    task, or it survived longer than ``reconnect.max_backoff_seconds``
    measured on the injected monotonic ``clock`` from successful registration
    to the drop; see :class:`aion_worker.ReconnectConfig`.
    """

    from .reconnect import reconnect_register_and_replay

    unacked = tracker if tracker is not None else UnackedResultTracker()
    activity_types = list(dispatcher.activity_types())
    backoff = ReconnectBackoff.from_config(config)
    dropped_attempt = 0
    while shutdown is None or not shutdown.is_set():
        session = await reconnect_register_and_replay(
            connect=connect,
            config=config,
            activity_types=activity_types,
            available_handlers=activity_types,
            tracker=unacked,
            sleep=sleep,
        )
        established_at = clock()
        health = SessionHealth()
        drop: BaseException
        try:
            logger.info("Connected")
            logger.info("Registered activities: %s", ", ".join(activity_types))
            logger.info("Waiting for tasks")
            end = await serve(config, session, dispatcher, unacked, shutdown, health)
            if isinstance(end, ShutdownRequested):
                return
            if shutdown is not None and shutdown.is_set():
                return
            logger.warning("Server closed the worker stream cleanly; treating it as a session drop")
            await _close_session(session)
            drop = ServerClosedStreamError(f"server closed the worker stream cleanly for {config.endpoint}")
        except Exception as exc:
            if not is_retryable_session_error(exc):
                logger.error(
                    "Worker was denied by the server (%s); not reconnecting",
                    grpc_status_code(exc),
                )
                await _close_session(session)
                raise
            logger.exception("worker session dropped; reconnecting before receiving more tasks")
            await _close_session(session)
            drop = exc
        if health.tasks_served > 0 or clock() - established_at > backoff.max_backoff_seconds:
            if dropped_attempt > 0:
                logger.info(
                    "Worker session proved healthy (%s tasks served); drop budget reset",
                    health.tasks_served,
                )
            dropped_attempt = 0
        dropped_attempt += 1
        if dropped_attempt >= backoff.max_attempts:
            raise ReconnectError(f"worker reconnect attempts exhausted for {config.endpoint}: {drop}") from drop
        delay = backoff.delay_for_attempt(dropped_attempt)
        logger.warning(
            "Reconnecting in %ss (attempt %s/%s)",
            delay,
            dropped_attempt,
            backoff.max_attempts,
        )
        await sleep(delay)


async def _receive_next_or_shutdown(
    stream: AsyncIterator[WorkerSessionEvent],
    shutdown: asyncio.Event | None,
) -> WorkerSessionEvent | ShutdownRequested | StreamFinished:
    next_task = asyncio.ensure_future(stream.__anext__())
    if shutdown is None:
        try:
            return await next_task
        except StopAsyncIteration:
            return StreamFinished()
    shutdown_task: asyncio.Task[bool] = asyncio.create_task(shutdown.wait())
    try:
        wait_tasks = cast(set[asyncio.Future[object]], {next_task, shutdown_task})
        done, pending = await asyncio.wait(wait_tasks, return_when=asyncio.FIRST_COMPLETED)
        for pending_task in pending:
            pending_task.cancel()
        if shutdown_task in done:
            next_task.cancel()
            return ShutdownRequested()
        try:
            return next_task.result()
        except StopAsyncIteration:
            return StreamFinished()
    finally:
        shutdown_task.cancel()


async def _close_session(session: WorkerSession) -> None:
    close = getattr(session, "close", None)
    if close is None:
        return
    try:
        result = close()
        if inspect.isawaitable(result):
            await result
    except Exception as exc:
        logger.warning("failed to close dropped worker session: %s", exc)


async def _run_and_report(
    session: WorkerSession,
    dispatcher: ActivityDispatcher,
    task: ActivityTask,
    tracker: UnackedResultTracker,
    semaphore: asyncio.Semaphore,
    in_flight: dict[_InFlightKey, ActivityCancellationHandle],
    health: SessionHealth | None,
) -> None:
    key = _in_flight_key(task.workflow_id, task.activity_id)
    try:
        logger.info(
            "Received task %s for workflow %s",
            task.activity_type,
            task.workflow_id.uuid,
            extra=_log_fields(task.workflow_id, task.activity_id, task.activity_type),
        )
        started_at = time.perf_counter()
        context = ActivityContext(
            workflow_id=task.workflow_id,
            activity_id=task.activity_id,
            attempt=WIRE_DEFAULT_ATTEMPT,
            session=session,
            content_type=task.input.content_type,
        )
        in_flight[key] = ActivityCancellationHandle(context)
        outcome = await dispatcher.dispatch(task, context)
        await _report_outcome(session, task, outcome, tracker)
        if health is not None:
            # A received task whose outcome report went out is the
            # end-to-end health proof used for drop-budget resets.
            health.tasks_served += 1
        elapsed_ms = round((time.perf_counter() - started_at) * 1000)
        if isinstance(outcome, Completed):
            logger.info(
                "Completed %s in %sms",
                task.activity_type,
                elapsed_ms,
                extra=_log_fields(task.workflow_id, task.activity_id, task.activity_type),
            )
        else:
            logger.error(
                "Failed %s for workflow %s in %sms: %s",
                task.activity_type,
                task.workflow_id.uuid,
                elapsed_ms,
                outcome.failure.message,
                extra=_log_fields(task.workflow_id, task.activity_id, task.activity_type),
            )
    except Exception as exc:
        logger.error(
            "Task %s for workflow %s failed: %s",
            task.activity_type,
            task.workflow_id.uuid,
            exc,
            extra=_log_fields(task.workflow_id, task.activity_id, task.activity_type),
        )
        raise
    finally:
        in_flight.pop(key, None)
        semaphore.release()


async def _report_outcome(
    session: WorkerSession, task: ActivityTask, outcome: DispatchOutcome, tracker: UnackedResultTracker
) -> None:
    if isinstance(outcome, Completed):
        report = PendingCompletedReport(task.workflow_id, task.activity_id, outcome.output)
        tracker.record(report)
        logger.info(
            "reporting activity completion",
            extra=_log_fields(task.workflow_id, task.activity_id, task.activity_type),
        )
        await session.report_result(task.workflow_id, task.activity_id, outcome.output)
        logger.info(
            "reported activity completion",
            extra=_log_fields(task.workflow_id, task.activity_id, task.activity_type),
        )
        return
    failed_report = PendingFailedReport(task.workflow_id, task.activity_id, outcome.failure)
    tracker.record(failed_report)
    logger.info("reporting activity failure", extra=_log_fields(task.workflow_id, task.activity_id, task.activity_type))
    await session.report_failure(task.workflow_id, task.activity_id, outcome.failure)
    logger.info("reported activity failure", extra=_log_fields(task.workflow_id, task.activity_id, task.activity_type))


def _handle_control_event(event: WorkerSessionEvent, in_flight: dict[_InFlightKey, ActivityCancellationHandle]) -> None:
    if isinstance(event, ActivityCancelled):
        logger.info(
            "received cooperative activity cancellation",
            extra={
                "workflow_id": event.workflow_id.uuid,
                "activity_id": event.activity_id.sequence_position,
            },
        )
        handle = in_flight.get(_in_flight_key(event.workflow_id, event.activity_id))
        if handle is not None:
            handle.cancel()


def _in_flight_key(workflow_id: WorkflowId, activity_id: ActivityId) -> _InFlightKey:
    return (workflow_id.uuid, activity_id.sequence_position)


def _log_fields(workflow_id: WorkflowId, activity_id: ActivityId, activity_type: str) -> dict[str, object]:
    return {
        "workflow_id": workflow_id.uuid,
        "activity_id": activity_id.sequence_position,
        "activity_type": activity_type,
    }
