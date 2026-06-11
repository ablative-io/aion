from __future__ import annotations

import asyncio
import time
from collections.abc import AsyncIterator, Iterable
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field

import pytest
from ar007_fakes import activity_id, config, workflow_id

import aion_worker
from aion_worker import ActivityCancelled, ActivityContext, ActivityRegistry, Worker, WorkerConfig
from aion_worker.proto import common_pb2, worker_pb2


@dataclass
class RecordingSession:
    events: asyncio.Queue[aion_worker.WorkerSessionEvent | None] = field(default_factory=asyncio.Queue)
    handshakes: list[tuple[str, str]] = field(default_factory=list)
    registrations: list[list[str]] = field(default_factory=list)
    results: list[tuple[int, common_pb2.Payload]] = field(default_factory=list)
    failures: list[tuple[int, worker_pb2.ActivityError]] = field(default_factory=list)
    heartbeats: list[tuple[int, common_pb2.Payload | None]] = field(default_factory=list)

    async def handshake(self, worker_config: WorkerConfig) -> None:
        self.handshakes.append((worker_config.task_queue, worker_config.identity))

    async def register(self, activity_types: Iterable[str], available_handlers: Iterable[str]) -> None:
        del available_handlers
        self.registrations.append(list(activity_types))

    def receive_tasks(self) -> AsyncIterator[aion_worker.WorkerSessionEvent]:
        return self._receive()

    async def push(self, event: aion_worker.WorkerSessionEvent) -> None:
        await self.events.put(event)

    async def finish(self) -> None:
        await self.events.put(None)

    async def report_result(
        self,
        workflow: common_pb2.WorkflowId,
        activity: common_pb2.ActivityId,
        result: common_pb2.Payload,
    ) -> None:
        del workflow
        self.results.append((activity.sequence_position, result))

    async def report_failure(
        self,
        workflow: common_pb2.WorkflowId,
        activity: common_pb2.ActivityId,
        failure: worker_pb2.ActivityError,
    ) -> None:
        del workflow
        self.failures.append((activity.sequence_position, failure))

    async def send_heartbeat(
        self,
        workflow: common_pb2.WorkflowId,
        activity: common_pb2.ActivityId,
        progress: common_pb2.Payload | None,
    ) -> None:
        del workflow
        self.heartbeats.append((activity.sequence_position, progress))

    async def _receive(self) -> AsyncIterator[aion_worker.WorkerSessionEvent]:
        while True:
            event = await self.events.get()
            if event is None:
                return
            yield event


def json_payload(value: object, content_type: str = "application/json") -> common_pb2.Payload:
    return aion_worker.encode_json_payload(value, content_type)


def typed_task(sequence_position: int, activity_type: str, value: object) -> aion_worker.TaskReceived:
    return aion_worker.TaskReceived(
        aion_worker.ActivityTask(
            workflow_id=workflow_id(),
            activity_id=activity_id(sequence_position),
            activity_type=activity_type,
            input=json_payload(value),
            attempt=1,
        )
    )


async def test_activity_registry_typed_json_round_trip_preserves_content_type() -> None:
    registry = ActivityRegistry()

    @registry.activity(name="double")
    async def double(value: int, ctx: ActivityContext) -> int:
        assert ctx.activity_id.sequence_position == 1
        return value * 2

    session = RecordingSession()
    context = ActivityContext(workflow_id=workflow_id(), activity_id=activity_id(1), attempt=1, session=session)
    outcome = await registry.dispatch(typed_task(1, "double", 21).task, context)

    assert isinstance(outcome, aion_worker.Completed)
    assert outcome.output.content_type == "application/json"
    assert aion_worker.decode_json_payload(outcome.output) == 42


def test_duplicate_activity_registration_fails_at_decoration_time() -> None:
    registry = ActivityRegistry()

    @registry.activity(name="same")
    async def first(value: int) -> int:
        return value

    with pytest.raises(aion_worker.ActivityRegistrationError, match="already registered"):

        @registry.activity(name="same")
        async def second(value: int) -> int:
            return value

    assert first.__name__ == "first"


async def test_non_json_input_is_terminal_decode_failure() -> None:
    registry = ActivityRegistry()

    @registry.activity(name="echo")
    async def echo(value: int) -> int:
        return value

    session = RecordingSession()
    context = ActivityContext(workflow_id=workflow_id(), activity_id=activity_id(1), attempt=1, session=session)
    bad_task = aion_worker.ActivityTask(
        workflow_id=workflow_id(),
        activity_id=activity_id(1),
        activity_type="echo",
        input=common_pb2.Payload(content_type="application/x-custom", bytes=b"1"),
        attempt=1,
    )

    outcome = await registry.dispatch(bad_task, context)

    assert isinstance(outcome, aion_worker.Failed)
    assert outcome.failure.kind == worker_pb2.ACTIVITY_ERROR_KIND_TERMINAL
    assert "unsupported payload content type" in outcome.failure.message


async def test_retryable_terminal_and_unclassified_failures_are_classified(
    caplog: pytest.LogCaptureFixture,
) -> None:
    registry = ActivityRegistry()

    @registry.activity(name="retry")
    async def retry(value: int) -> int:
        del value
        raise aion_worker.RetryableError("try again", detail={"retry": True})

    @registry.activity(name="terminal")
    async def terminal(value: int) -> int:
        del value
        raise aion_worker.TerminalError("stop", detail={"terminal": True})

    @registry.activity(name="bare")
    async def bare(value: int) -> int:
        del value
        raise ValueError("boom")

    session = RecordingSession()
    context = ActivityContext(workflow_id=workflow_id(), activity_id=activity_id(1), attempt=1, session=session)

    retry_outcome = await registry.dispatch(typed_task(1, "retry", 0).task, context)
    terminal_outcome = await registry.dispatch(typed_task(1, "terminal", 0).task, context)
    with caplog.at_level("WARNING"):
        bare_outcome = await registry.dispatch(typed_task(1, "bare", 0).task, context)

    assert isinstance(retry_outcome, aion_worker.Failed)
    assert retry_outcome.failure.kind == worker_pb2.ACTIVITY_ERROR_KIND_RETRYABLE
    assert retry_outcome.failure.message == "try again"
    assert aion_worker.decode_json_payload(retry_outcome.failure.details) == {"retry": True}
    assert isinstance(terminal_outcome, aion_worker.Failed)
    assert terminal_outcome.failure.kind == worker_pb2.ACTIVITY_ERROR_KIND_TERMINAL
    assert aion_worker.decode_json_payload(terminal_outcome.failure.details) == {"terminal": True}
    assert isinstance(bare_outcome, aion_worker.Failed)
    assert bare_outcome.failure.kind == worker_pb2.ACTIVITY_ERROR_KIND_RETRYABLE
    assert bare_outcome.failure.message == "boom"
    assert any("unclassified" in record.getMessage() for record in caplog.records)


async def test_explicit_heartbeat_only_sends_when_handler_calls_context() -> None:
    registry = ActivityRegistry()

    @registry.activity(name="beat")
    async def beat(value: int, ctx: ActivityContext) -> int:
        await ctx.heartbeat({"at": value})
        return value

    session = RecordingSession()
    worker = Worker(config(), registry=registry)
    await session.push(typed_task(1, "beat", 3))
    await session.finish()

    await worker.run_with_session(session)

    assert len(session.heartbeats) == 1
    assert session.heartbeats[0][0] == 1
    heartbeat_payload = session.heartbeats[0][1]
    assert heartbeat_payload is not None
    assert aion_worker.decode_json_payload(heartbeat_payload) == {"at": 3}
    assert len(session.results) == 1


async def test_cooperative_cancellation_marks_context_without_forcing_handler() -> None:
    registry = ActivityRegistry()
    cancellation_seen = asyncio.Event()
    allow_return = asyncio.Event()

    @registry.activity(name="slow")
    async def slow(value: int, ctx: ActivityContext) -> int:
        del value
        await ctx.cancelled()
        assert ctx.is_cancelled()
        cancellation_seen.set()
        await allow_return.wait()
        return 99

    session = RecordingSession()
    worker = Worker(config(max_concurrency=1), registry=registry)
    await session.push(typed_task(1, "slow", 0))
    await session.push(ActivityCancelled(workflow_id(), activity_id(1)))
    serving = asyncio.create_task(worker.run_with_session(session))

    await asyncio.wait_for(cancellation_seen.wait(), timeout=1)
    assert session.results == []
    allow_return.set()
    await session.finish()
    await serving

    assert aion_worker.decode_json_payload(session.results[0][1]) == 99


async def test_worker_graceful_shutdown_drains_in_flight_task_before_returning() -> None:
    registry = ActivityRegistry()
    started = asyncio.Event()
    release = asyncio.Event()

    @registry.activity(name="drain")
    async def drain(value: int) -> int:
        started.set()
        await release.wait()
        return value + 1

    session = RecordingSession()
    shutdown = asyncio.Event()
    worker = Worker(config(), registry=registry)
    await session.push(typed_task(1, "drain", 10))
    serving = asyncio.create_task(worker.run_with_session(session, shutdown=shutdown))

    await asyncio.wait_for(started.wait(), timeout=1)
    shutdown.set()
    await asyncio.sleep(0.02)
    assert not serving.done()

    release.set()
    await serving
    assert aion_worker.decode_json_payload(session.results[0][1]) == 11


async def test_sync_handler_runs_in_executor_without_stalling_async_handler() -> None:
    registry = ActivityRegistry(executor=ThreadPoolExecutor(max_workers=1))
    async_finished = asyncio.Event()

    @registry.activity(name="blocking")
    def blocking(value: int) -> int:
        time.sleep(0.1)
        return value

    @registry.activity(name="async")
    async def async_handler(value: int) -> int:
        async_finished.set()
        return value + 1

    session = RecordingSession()
    worker = Worker(config(max_concurrency=2), registry=registry)
    await session.push(typed_task(1, "blocking", 1))
    await session.push(typed_task(2, "async", 1))
    await session.finish()

    serving = asyncio.create_task(worker.run_with_session(session))
    await asyncio.wait_for(async_finished.wait(), timeout=0.05)
    await serving

    decoded_results = {sequence: aion_worker.decode_json_payload(result) for sequence, result in session.results}
    assert decoded_results == {1: 1, 2: 2}
