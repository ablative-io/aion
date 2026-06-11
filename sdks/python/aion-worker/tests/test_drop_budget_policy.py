"""Cross-SDK drop-budget policy: budget resets and clean-close reconnects."""

from __future__ import annotations

import asyncio
from collections.abc import AsyncIterator, Iterable

import grpc
import pytest
from ar007_fakes import (
    FakeSession,
    RecordingDispatcher,
    activity_id,
    payload,
    task,
    wait_for_condition,
    workflow_id,
)

from aion_worker import (
    ActivityExecutionContext,
    ActivityTask,
    Completed,
    DispatchOutcome,
    PendingCompletedReport,
    ReconnectConfig,
    ReconnectError,
    ServerClosedStreamError,
    UnackedResultTracker,
    WorkerConfig,
    WorkerSessionEvent,
    connect_register_replay_and_serve,
)
from aion_worker.proto import common_pb2, worker_pb2


def _config(max_attempts: int) -> WorkerConfig:
    return WorkerConfig(
        endpoint="http://127.0.0.1:50051",
        task_queue="queue-a",
        identity="worker-a",
        max_concurrency=2,
        reconnect=ReconnectConfig(
            initial_backoff_seconds=0.01,
            max_backoff_seconds=0.02,
            max_attempts=max_attempts,
        ),
    )


async def _no_sleep(delay: float) -> None:
    del delay


def _frozen_clock() -> float:
    """Monotonic clock that never advances: time-based resets cannot fire."""

    return 0.0


async def _dropping_session(sequence_position: int | None = None) -> FakeSession:
    """Session that serves its optional task then drops with an OSError."""

    session = FakeSession(drop_after_events=True)
    if sequence_position is not None:
        await session.push(task(sequence_position))
    await session.finish()
    return session


async def test_drop_budget_resets_after_each_session_that_serves_a_task() -> None:
    dispatcher = RecordingDispatcher()
    blocking = FakeSession()
    attempts = 0

    async def connect() -> FakeSession:
        nonlocal attempts
        attempts += 1
        if attempts <= 5:
            return await _dropping_session(attempts)
        return blocking

    shutdown = asyncio.Event()
    run = asyncio.create_task(
        connect_register_replay_and_serve(
            _config(max_attempts=2),
            connect,
            dispatcher,
            sleep=_no_sleep,
            shutdown=shutdown,
            clock=_frozen_clock,
        )
    )
    # Wait until the worker has survived all five drops and dialled the
    # sixth (blocking) session; run.done() guards against a budget
    # exhaustion ending the run early.
    while attempts < 6 and not run.done():
        await asyncio.sleep(0)
    shutdown.set()
    await run

    # Five sessions each served one task and then dropped. With a budget of
    # max_attempts=2 the run would have ended at the second drop without the
    # reset rule; every served task reset the budget, so the worker kept
    # recovering until the graceful shutdown. The frozen clock guarantees the
    # time-based reset never contributed.
    assert attempts == 6
    assert dispatcher.dispatched == [1, 2, 3, 4, 5]


async def test_drop_budget_resets_when_a_session_outlives_max_backoff() -> None:
    # clock() is read twice per session: once at establishment, once at the
    # drop. Session two "survives" 10s against max_backoff_seconds=0.02; the
    # others drop instantly.
    times = iter([0.0, 0.0, 0.0, 10.0, 10.0, 10.0])

    def scripted_clock() -> float:
        return next(times)

    attempts = 0

    async def connect() -> FakeSession:
        nonlocal attempts
        attempts += 1
        return await _dropping_session()

    with pytest.raises(ReconnectError) as exhausted:
        await connect_register_replay_and_serve(
            _config(max_attempts=2),
            connect,
            RecordingDispatcher(),
            sleep=_no_sleep,
            clock=scripted_clock,
        )

    # Drop one consumed the first budget unit; session two served no tasks
    # but outlived max_backoff, so its drop restarted the count at one; the
    # third session's instant drop was the second post-reset unit and
    # exhausted max_attempts=2 — proving exactly one unit was consumed before
    # the reset. Without the reset the run would have ended after 2 sessions.
    assert attempts == 3
    assert isinstance(exhausted.value.__cause__, OSError)


async def test_flapping_sessions_exhaust_budget_at_exactly_max_attempts() -> None:
    attempts = 0

    async def connect() -> FakeSession:
        nonlocal attempts
        attempts += 1
        return await _dropping_session()

    with pytest.raises(ReconnectError) as exhausted:
        await connect_register_replay_and_serve(
            _config(max_attempts=3),
            connect,
            RecordingDispatcher(),
            sleep=_no_sleep,
            clock=_frozen_clock,
        )

    # No session served a task and the frozen clock keeps lifetimes at zero,
    # so no reset fires: the budget exhausts at exactly max_attempts drops
    # (cross-SDK accounting parity) with no further dial after the last drop.
    assert attempts == 3
    assert isinstance(exhausted.value.__cause__, OSError)


async def test_clean_server_close_reconnects_re_registers_and_keeps_serving() -> None:
    dispatcher = RecordingDispatcher()
    first = FakeSession()
    await first.push(task(1))
    await first.finish()
    second = FakeSession()
    await second.push(task(2))
    sessions = [first, second]
    attempts = 0

    async def connect() -> FakeSession:
        nonlocal attempts
        attempts += 1
        return sessions.pop(0)

    shutdown = asyncio.Event()
    run = asyncio.create_task(
        connect_register_replay_and_serve(
            _config(max_attempts=3),
            connect,
            dispatcher,
            sleep=_no_sleep,
            shutdown=shutdown,
        )
    )
    await wait_for_condition(run, lambda: dispatcher.dispatched == [1, 2])
    shutdown.set()
    await run

    # The first session's clean close was a retryable drop: the worker
    # redialled, re-registered, kept serving, and shutdown still returned
    # cleanly and promptly — closing the live session on the way out.
    assert attempts == 2
    assert first.closed is True
    assert second.handshakes == [("queue-a", "worker-a")]
    assert second.registrations == [["slow"]]
    assert dispatcher.dispatched == [1, 2]
    assert second.closed is True


async def test_persistent_clean_close_loop_exhausts_budget_with_classified_error() -> None:
    attempts = 0

    async def connect() -> FakeSession:
        nonlocal attempts
        attempts += 1
        session = FakeSession()
        await session.finish()
        return session

    with pytest.raises(ReconnectError) as exhausted:
        await connect_register_replay_and_serve(
            _config(max_attempts=2),
            connect,
            RecordingDispatcher(),
            sleep=_no_sleep,
            clock=_frozen_clock,
        )

    # Clean closes consume the same budget as error drops and exhaust with a
    # classified clean-close cause.
    assert attempts == 2
    assert isinstance(exhausted.value.__cause__, ServerClosedStreamError)


class _ReplayFailingSession(FakeSession):
    """Session whose report writes fail the way a dead transport's do."""

    async def report_result(
        self,
        workflow: common_pb2.WorkflowId,
        activity: common_pb2.ActivityId,
        result: common_pb2.Payload,
    ) -> None:
        raise OSError("replay write lost the race with a second reset")

    async def report_failure(
        self,
        workflow: common_pb2.WorkflowId,
        activity: common_pb2.ActivityId,
        failure: worker_pb2.ActivityError,
    ) -> None:
        raise OSError("replay write lost the race with a second reset")


class _DeniedReplaySession(FakeSession):
    """Session whose report writes are denied deterministically by the server."""

    def __init__(self, denial: grpc.aio.AioRpcError) -> None:
        super().__init__()
        self.denial = denial

    async def report_result(
        self,
        workflow: common_pb2.WorkflowId,
        activity: common_pb2.ActivityId,
        result: common_pb2.Payload,
    ) -> None:
        raise self.denial

    async def report_failure(
        self,
        workflow: common_pb2.WorkflowId,
        activity: common_pb2.ActivityId,
        failure: worker_pb2.ActivityError,
    ) -> None:
        raise self.denial


def _unacked_tracker_with_one_report() -> UnackedResultTracker:
    tracker = UnackedResultTracker()
    tracker.record(
        PendingCompletedReport(
            workflow_id=workflow_id(),
            activity_id=activity_id(7),
            output=payload(),
        )
    )
    return tracker


async def test_retryable_replay_failure_counts_against_drop_budget() -> None:
    tracker = _unacked_tracker_with_one_report()
    replay_failing = _ReplayFailingSession()
    third = FakeSession()
    await third.finish()
    attempts = 0

    async def connect() -> FakeSession:
        nonlocal attempts
        attempts += 1
        if attempts == 1:
            return await _dropping_session()
        if attempts == 2:
            return replay_failing
        return third

    with pytest.raises(ReconnectError) as exhausted:
        await connect_register_replay_and_serve(
            _config(max_attempts=3),
            connect,
            RecordingDispatcher(),
            tracker,
            sleep=_no_sleep,
            clock=_frozen_clock,
        )

    # Drop one: the stream reset. Drop two: the failed unacked-result replay
    # on the second session (closed before re-entering the cycle) — a
    # budgeted, retryable drop rather than an instant run failure. The third
    # session then received the replayed result before its own clean close
    # exhausted the budget, proving replay re-entry shares the one
    # cumulative budget.
    assert attempts == 3
    assert replay_failing.closed is True
    assert "result:7" in third.log
    assert isinstance(exhausted.value.__cause__, ServerClosedStreamError)


async def test_denial_during_replay_fails_fast_with_precedence() -> None:
    tracker = _unacked_tracker_with_one_report()
    denial = grpc.aio.AioRpcError(
        grpc.StatusCode.PERMISSION_DENIED,
        grpc.aio.Metadata(),
        grpc.aio.Metadata(),
        details="namespace 'queue-a' was revoked",
    )
    denied = _DeniedReplaySession(denial)
    attempts = 0

    async def connect() -> FakeSession:
        nonlocal attempts
        attempts += 1
        if attempts == 1:
            return await _dropping_session()
        return denied

    with pytest.raises(grpc.aio.AioRpcError) as raised:
        await connect_register_replay_and_serve(
            _config(max_attempts=5),
            connect,
            RecordingDispatcher(),
            tracker,
            sleep=_no_sleep,
            clock=_frozen_clock,
        )

    # The deterministic denial during replay outranks the remaining budget:
    # no further reconnects, session closed, denial surfaced raw.
    assert attempts == 2
    assert raised.value.code() is grpc.StatusCode.PERMISSION_DENIED
    assert denied.closed is True


class _DrainBlockedDispatcher:
    """Dispatcher whose single handler blocks until the test releases it."""

    def __init__(self, release: asyncio.Event) -> None:
        self.release = release
        self.finished = False

    def activity_types(self) -> Iterable[str]:
        return ["slow"]

    async def dispatch(self, task: ActivityTask, context: ActivityExecutionContext) -> DispatchOutcome:
        del task, context
        await self.release.wait()
        self.finished = True
        return Completed(payload())


class _ReleaseOnDropSession(FakeSession):
    """Drops its stream, releasing the blocked handler only at the drop.

    The release is deferred by one event-loop turn behind the raise, so the
    serve loop's continuation (which captures the stream-end timestamp in its
    ``finally`` before awaiting in-flight tasks) is scheduled ahead of the
    handler's wakeup: the handler can only resume during the post-drop drain.
    Reports fail so the drained task never counts as served.
    """

    def __init__(self, release: asyncio.Event) -> None:
        super().__init__()
        self._release = release

    async def _receive(self) -> AsyncIterator[WorkerSessionEvent]:
        while True:
            event = await self.events.get()
            if event is None:
                asyncio.get_running_loop().call_soon(self._release.set)
                raise OSError("stream dropped")
            self.log.append("receive")
            yield event

    async def report_result(
        self,
        workflow: common_pb2.WorkflowId,
        activity: common_pb2.ActivityId,
        result: common_pb2.Payload,
    ) -> None:
        raise OSError("report send failed after the drop")


async def test_drain_outliving_max_backoff_does_not_reset_drop_budget() -> None:
    """Connected time is measured to the stream end, never to the drain end.

    Degenerate scenario: the server dispatches a long task and kills the
    stream almost immediately; the report fails so no task counts as served.
    If the elapsed-connected measurement included the post-drop drain, every
    such cycle would outlive ``max_backoff_seconds``, reset the budget, and
    flap forever instead of exhausting.
    """

    release = asyncio.Event()
    dispatcher = _DrainBlockedDispatcher(release)

    def drain_aware_clock() -> float:
        # Virtual time advances (far past max_backoff_seconds=0.02) only once
        # the draining handler finishes: a decision measured at the stream
        # end sees 0.0, one measured after the drain sees 100.0.
        return 100.0 if dispatcher.finished else 0.0

    attempts = 0

    async def connect() -> FakeSession:
        nonlocal attempts
        attempts += 1
        if attempts == 2:
            session = _ReleaseOnDropSession(release)
            await session.push(task(1))
            await session.finish()
            return session
        return await _dropping_session()

    with pytest.raises(ReconnectError) as exhausted:
        await connect_register_replay_and_serve(
            _config(max_attempts=2),
            connect,
            dispatcher,
            sleep=_no_sleep,
            clock=drain_aware_clock,
        )

    # Cycle one consumed the first budget unit. Cycle two dispatched a task,
    # dropped its stream at a connected lifetime of 0.0, then spent "100s"
    # draining the in-flight handler whose report failed. Measured to the
    # stream end the session never proved healthy, so the second drop
    # exhausted max_attempts=2; measured to the drain end it would have reset
    # the budget and dialled a third session.
    assert attempts == 2
    assert dispatcher.finished is True
    assert isinstance(exhausted.value.__cause__, OSError)


async def test_shutdown_during_error_backoff_wakes_immediately() -> None:
    """Shutdown is raced against the drop-backoff sleep, not observed after it."""

    shutdown = asyncio.Event()
    backoff_entered = asyncio.Event()
    attempts = 0

    async def connect() -> FakeSession:
        nonlocal attempts
        attempts += 1
        return await _dropping_session()

    async def hanging_sleep(delay: float) -> None:
        del delay
        backoff_entered.set()
        # Stands in for an arbitrarily long backoff: it never completes, so
        # only the shutdown race can end the wait.
        await asyncio.Event().wait()

    run = asyncio.create_task(
        connect_register_replay_and_serve(
            _config(max_attempts=5),
            connect,
            RecordingDispatcher(),
            sleep=hanging_sleep,
            shutdown=shutdown,
            clock=_frozen_clock,
        )
    )
    await asyncio.wait_for(backoff_entered.wait(), timeout=5)
    shutdown.set()
    # Returns cleanly well before any backoff could elapse — the timeout only
    # fires if the worker stalls waiting out the sleep.
    await asyncio.wait_for(run, timeout=5)

    assert attempts == 1


async def test_shutdown_during_clean_close_backoff_returns_cleanly() -> None:
    shutdown = asyncio.Event()
    attempts = 0

    async def connect() -> FakeSession:
        nonlocal attempts
        attempts += 1
        session = FakeSession()
        await session.finish()
        return session

    async def shutdown_during_backoff(delay: float) -> None:
        del delay
        shutdown.set()

    # The first clean close enters the drop backoff; shutdown fires during
    # the backoff sleep and the run returns cleanly without redialling.
    await connect_register_replay_and_serve(
        _config(max_attempts=3),
        connect,
        RecordingDispatcher(),
        sleep=shutdown_during_backoff,
        shutdown=shutdown,
    )

    assert attempts == 1
