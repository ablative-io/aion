"""Cross-SDK drop-budget policy: budget resets and clean-close reconnects."""

from __future__ import annotations

import asyncio

import pytest
from ar007_fakes import FakeSession, RecordingDispatcher, task

from aion_worker import (
    ReconnectConfig,
    ReconnectError,
    ServerClosedStreamError,
    WorkerConfig,
    connect_register_replay_and_serve,
)


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
    while dispatcher.dispatched != [1, 2]:
        await asyncio.sleep(0)
    shutdown.set()
    await run

    # The first session's clean close was a retryable drop: the worker
    # redialled, re-registered, kept serving, and shutdown still returned
    # cleanly and promptly.
    assert attempts == 2
    assert first.closed is True
    assert second.handshakes == [("queue-a", "worker-a")]
    assert second.registrations == [["slow"]]
    assert dispatcher.dispatched == [1, 2]


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
