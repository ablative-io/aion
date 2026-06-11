"""Asyncio receive, dispatch, and report loop with bounded concurrency."""

from __future__ import annotations

import asyncio
import inspect
import logging
import time
from collections.abc import AsyncIterator, Awaitable, Callable, Iterable
from dataclasses import dataclass
from typing import Protocol, TypeAlias, cast

from .context import ActivityCancellationHandle, ActivityContext
from .reconnect import (
    PendingCompletedReport,
    PendingFailedReport,
    ReconnectBackoff,
    ReconnectError,
    ServerClosedStreamError,
    UnackedResultTracker,
    grpc_status_code,
    is_retryable_session_error,
    re_report_unacked,
    reconnect_with_backoff,
    sleep_or_shutdown,
)
from .session import (
    ActivityCancelled,
    ActivityError,
    ActivityId,
    ActivityTask,
    DrainReceived,
    Payload,
    ResultAcknowledged,
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

    The reconnect-aware run loop treats this unannounced close as a budgeted
    retryable session drop — never as a run end — so workers ride through
    graceful server closes.
    """


class Drained:
    """Sentinel returned when the server announced a drain.

    In-flight work was finished and reported; the run loop reconnects after
    the schedule's initial backoff without consuming any drop budget.
    """


@dataclass
class SessionHealth:
    """Per-session liveness counters used for drop-budget reset accounting."""

    tasks_served: int = 0
    stream_ended_at: float | None = None
    """Clock reading taken the moment the session's receive stream ended,
    captured before in-flight handlers are drained — so post-drop draining
    never extends the session's measured connected lifetime."""
    drain_received: bool = False
    """Latched when a drain frame is observed on this session: the eventual
    stream end — clean OR abrupt — is then drain-class (the server announced
    it was going away), so the drop consumes no budget even if the post-drain
    reporting fails. Survives an error exit because health is an
    out-parameter."""


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
    clock: Callable[[], float] = time.monotonic,
) -> ShutdownRequested | StreamFinished | Drained:
    """Serve tasks from an already connected and registered session.

    Returns :class:`ShutdownRequested` when the caller's shutdown event ended
    the loop, :class:`StreamFinished` when the server ended the task stream
    cleanly without announcing a drain (the reconnect-aware caller treats
    that unannounced close as a budgeted retryable session drop), or
    :class:`Drained` when the server announced a drain (an unbudgeted drop;
    in-flight work is finished and reported before this returns). When
    ``health`` is supplied, every task whose outcome report was sent on this
    session increments ``health.tasks_served`` — the end-to-end proof used
    for drop-budget resets, surviving even when a later failure ends the
    session — ``health.stream_ended_at`` records the ``clock`` reading taken
    the moment the receive stream ended, before in-flight handlers are
    drained, and ``health.drain_received`` latches the drain classification.
    Server ``ResultAck`` events clear their unacked-tracker entries without
    occupying a concurrency slot.
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
            elif isinstance(event, ResultAcknowledged):
                # Acks are bookkeeping, not work: handled without acquiring
                # a concurrency slot. An unknown ack is a logged no-op.
                _acknowledge_result(event, unacked)
            elif isinstance(event, DrainReceived):
                logger.info("Server drain received; finishing in-flight work before reconnect")
                if health is not None:
                    health.drain_received = True
                return Drained()
            else:
                _handle_control_event(event, in_flight)
    finally:
        if health is not None and health.stream_ended_at is None:
            # The stream just ended — cleanly, by error, or by shutdown.
            # Capture the moment before draining in-flight handlers so the
            # caller's drop-budget reset decision measures connected time,
            # never drain time. No awaits may precede this capture.
            health.stream_ended_at = clock()
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
    (``reconnect.max_attempts``). An *unannounced* clean server stream close
    is a budgeted retryable drop: the worker redials through the same
    budgeted, backed-off cycle, so routine server deploys are ridden
    through. A server-announced drain is an UNBUDGETED drop: the worker
    finishes in-flight work and redials after ``initial_backoff_seconds``;
    the drain classification latches for the session, so even an abrupt end
    after the drain frame stays drain-class. A retryable failure while
    replaying unacknowledged results consumes the budget (the worker never
    saw a drain announcement), while a server denial during replay still
    fails fast. The budget resets to zero once an established session proves
    healthy — it served at least one task, or it stayed connected longer
    than ``reconnect.max_backoff_seconds`` measured on the injected
    monotonic ``clock`` from successful registration to the moment the
    stream ended (post-drop draining of in-flight handlers never extends
    it); see :class:`aion_worker.ReconnectConfig`. Shutdown wins promptly
    during BOTH backoff phases AND during an in-flight dial or the
    unacked-result replay: a shutdown requested during a mid-run drop
    backoff or during an establishment-retry backoff inside
    :func:`aion_worker.reconnect_with_backoff` wakes the sleep immediately,
    a shutdown requested during a hung dial/handshake/register cancels the
    in-flight attempt (closing its partially-established session), and a
    shutdown requested during a hung replay send abandons the replay
    (results stay tracked; entries are recorded before any send). The run
    outcome at shutdown follows the cross-SDK rule: a pending drain-class or
    clean-close drop returns cleanly, while a pending error-class drop
    re-raises its error.
    """

    unacked = tracker if tracker is not None else UnackedResultTracker()
    activity_types = list(dispatcher.activity_types())
    backoff = ReconnectBackoff.from_config(config)
    dropped_attempt = 0
    while shutdown is None or not shutdown.is_set():
        session = await reconnect_with_backoff(
            connect=connect,
            config=config,
            activity_types=activity_types,
            available_handlers=activity_types,
            backoff=backoff,
            shutdown=shutdown,
            sleep=sleep,
        )
        if session is None:
            # Shutdown fired during the establishment cycle (an
            # establishment-backoff sleep or an in-flight dial): return
            # cleanly, mirroring the drop-backoff shutdown path. No session
            # exists at this point — failed attempts close themselves and a
            # cancelled in-flight attempt closes its partial session inside
            # reconnect_with_backoff.
            return
        established_at = clock()
        health = SessionHealth()
        # None = drain-class drop (unbudgeted); ServerClosedStreamError =
        # unannounced clean close (budgeted); anything else = error-class.
        drop: BaseException | None
        try:
            replay_completed = await _replay_or_shutdown(session, unacked, shutdown)
            if not replay_completed:
                # Shutdown interrupted the replay: results stay tracked
                # (entries are recorded before any send and only an explicit
                # ack removes them), so nothing is lost by abandoning it.
                await _close_session(session)
                return
            logger.info("Connected")
            logger.info("Registered activities: %s", ", ".join(activity_types))
            logger.info("Waiting for tasks")
            end = await serve(config, session, dispatcher, unacked, shutdown, health, clock)
            if isinstance(end, ShutdownRequested) or (shutdown is not None and shutdown.is_set()):
                await _close_session(session)
                return
            if isinstance(end, Drained):
                logger.info("Server drained the worker stream; reconnecting after initial backoff")
                await _close_session(session)
                drop = None
            else:
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
            if health.drain_received:
                # Drain latch: the server announced it was going away, so the
                # abrupt end (or a failed post-drain report) is drain-class.
                logger.warning("Session error after server drain; classified as drain drop: %s", exc)
                await _close_session(session)
                drop = None
            else:
                logger.exception("worker session dropped; reconnecting before receiving more tasks")
                await _close_session(session)
                drop = exc
        # Connected lifetime is measured to the moment the stream ended —
        # never to the end of the post-drop drain, which would let a
        # long-running in-flight handler masquerade as a healthy session. A
        # replay failure never enters the serve loop, so its drop moment is
        # now.
        stream_ended_at = health.stream_ended_at if health.stream_ended_at is not None else clock()
        if health.tasks_served > 0 or stream_ended_at - established_at > backoff.max_backoff_seconds:
            if dropped_attempt > 0:
                logger.info(
                    "Worker session proved healthy (%s tasks served); drop budget reset",
                    health.tasks_served,
                )
            dropped_attempt = 0
        if drop is None:
            # An announced drain consumes no drop budget: the server told the
            # worker it was going away, so the drop is expected operator
            # behaviour, not flapping.
            delay = backoff.initial_backoff_seconds
        else:
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
        await sleep_or_shutdown(sleep, delay, shutdown)
        if shutdown is not None and shutdown.is_set():
            # Cross-SDK shutdown-outcome rule: a pending drain-class or
            # clean-close drop ends the run cleanly; a pending error-class
            # drop surfaces its error.
            if drop is not None and not isinstance(drop, ServerClosedStreamError):
                raise drop
            return


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


async def _replay_or_shutdown(
    session: WorkerSession,
    tracker: UnackedResultTracker,
    shutdown: asyncio.Event | None,
) -> bool:
    """Race the unacked-result replay against shutdown.

    A hung replay send must never wedge worker shutdown. Returns ``False``
    when shutdown won (the replay task is cancelled and awaited; tracked
    results survive because entries are recorded before any send), ``True``
    when the replay completed. Replay failures propagate unchanged.
    """

    if shutdown is None:
        await re_report_unacked(session, tracker)
        return True
    if shutdown.is_set():
        return False
    replay_task = asyncio.ensure_future(re_report_unacked(session, tracker))
    shutdown_task: asyncio.Task[bool] = asyncio.create_task(shutdown.wait())
    try:
        wait_tasks = cast(set[asyncio.Future[object]], {replay_task, shutdown_task})
        done, _pending = await asyncio.wait(wait_tasks, return_when=asyncio.FIRST_COMPLETED)
        if replay_task in done:
            replay_task.result()
            return True
        logger.info("Shutdown requested during unacked-result replay; abandoning the replay")
        replay_task.cancel()
        try:
            await replay_task
        except asyncio.CancelledError:
            if not replay_task.cancelled():
                # The CancelledError is the enclosing coroutine's own
                # cancellation, not the replay acknowledging ours.
                raise
        except Exception as exc:
            logger.warning("unacked-result replay abandoned at shutdown failed: %s", exc)
        return False
    finally:
        shutdown_task.cancel()
        replay_task.cancel()


def _acknowledge_result(event: ResultAcknowledged, tracker: UnackedResultTracker) -> None:
    """Clear the acknowledged tracker entry; an unknown ack is a no-op."""

    if tracker.get(event.workflow_id, event.activity_id) is not None:
        tracker.acknowledge(event.workflow_id, event.activity_id)
        logger.debug(
            "server acknowledged activity result; tracker entry cleared",
            extra={
                "workflow_id": event.workflow_id.uuid,
                "activity_id": event.activity_id.sequence_position,
            },
        )
    else:
        logger.debug(
            "result ack for unknown tracker entry ignored",
            extra={
                "workflow_id": event.workflow_id.uuid,
                "activity_id": event.activity_id.sequence_position,
            },
        )


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
            "Received task %s for workflow %s (attempt %s)",
            task.activity_type,
            task.workflow_id.uuid,
            task.attempt,
            extra={
                **_log_fields(task.workflow_id, task.activity_id, task.activity_type),
                "attempt": task.attempt,
            },
        )
        started_at = time.perf_counter()
        context = ActivityContext(
            workflow_id=task.workflow_id,
            activity_id=task.activity_id,
            attempt=task.attempt,
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
