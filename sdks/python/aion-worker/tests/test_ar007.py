from __future__ import annotations

import asyncio
from collections.abc import AsyncIterator, Iterable
from dataclasses import dataclass, field

import aion_worker
from aion_worker import (
    ActivityExecutionContext,
    ActivityTask,
    Completed,
    DispatchOutcome,
    ReconnectConfig,
    TaskReceived,
    UnackedResultTracker,
    WorkerConfig,
    WorkerSessionEvent,
    connect_register_replay_and_serve,
    serve,
)
from aion_worker.proto import common_pb2, worker_pb2


def payload() -> common_pb2.Payload:
    return common_pb2.Payload(content_type="application/json", bytes=b"{}")


def workflow_id() -> common_pb2.WorkflowId:
    return common_pb2.WorkflowId(uuid="workflow-1")


def activity_id(sequence_position: int) -> common_pb2.ActivityId:
    return common_pb2.ActivityId(sequence_position=sequence_position)


def config(max_concurrency: int = 2) -> WorkerConfig:
    return WorkerConfig(
        endpoint="http://127.0.0.1:50051",
        task_queue="queue-a",
        identity="worker-a",
        max_concurrency=max_concurrency,
        reconnect=ReconnectConfig(initial_backoff_seconds=0.01, max_backoff_seconds=0.02, max_attempts=3),
    )


@dataclass
class FakeSession:
    events: asyncio.Queue[WorkerSessionEvent | None] = field(default_factory=asyncio.Queue)
    log: list[str] = field(default_factory=list)
    handshakes: list[tuple[str, str]] = field(default_factory=list)
    registrations: list[list[str]] = field(default_factory=list)

    async def handshake(self, worker_config: WorkerConfig) -> None:
        self.handshakes.append((worker_config.task_queue, worker_config.identity))
        self.log.append("handshake")

    async def register(self, activity_types: Iterable[str], available_handlers: Iterable[str]) -> None:
        registered = list(activity_types)
        missing = [name for name in registered if name not in set(available_handlers)]
        if missing:
            raise AssertionError(f"missing fake handler for {missing[0]}")
        self.registrations.append(registered)
        self.log.append("register")

    async def push(self, event: WorkerSessionEvent) -> None:
        await self.events.put(event)

    async def finish(self) -> None:
        await self.events.put(None)

    async def report_result(
        self,
        workflow: common_pb2.WorkflowId,
        activity: common_pb2.ActivityId,
        result: common_pb2.Payload,
    ) -> None:
        self.log.append(f"result:{activity.sequence_position}")

    async def report_failure(
        self,
        workflow: common_pb2.WorkflowId,
        activity: common_pb2.ActivityId,
        failure: worker_pb2.ActivityError,
    ) -> None:
        self.log.append(f"failure:{activity.sequence_position}")

    async def send_heartbeat(
        self,
        workflow: common_pb2.WorkflowId,
        activity: common_pb2.ActivityId,
        progress: common_pb2.Payload | None,
    ) -> None:
        self.log.append(f"heartbeat:{activity.sequence_position}")

    def receive_tasks(self) -> AsyncIterator[WorkerSessionEvent]:
        return self._receive()

    async def _receive(self) -> AsyncIterator[WorkerSessionEvent]:
        while True:
            event = await self.events.get()
            if event is None:
                return
            self.log.append("receive")
            yield event


class RecordingDispatcher:
    def __init__(self, release: asyncio.Event | None = None) -> None:
        self.active = 0
        self.peak = 0
        self.started = 0
        self.release = release
        self.dispatched: list[int] = []

    def activity_types(self) -> Iterable[str]:
        return ["slow"]

    async def dispatch(self, task: ActivityTask, context: ActivityExecutionContext) -> DispatchOutcome:
        self.active += 1
        self.peak = max(self.peak, self.active)
        self.started += 1
        self.dispatched.append(task.activity_id.sequence_position)
        try:
            if self.release is not None:
                await self.release.wait()
            return Completed(payload())
        finally:
            self.active -= 1


def task(sequence_position: int) -> TaskReceived:
    return TaskReceived(
        ActivityTask(
            workflow_id=workflow_id(),
            activity_id=activity_id(sequence_position),
            activity_type="slow",
            input=payload(),
        )
    )


def test_import_smoke_public_names() -> None:
    assert "WorkerConfig" in aion_worker.__all__
    assert "GrpcWorkerSession" in aion_worker.__all__
    assert "serve" in aion_worker.__all__


async def test_fake_session_handshake_and_registration_carry_configured_values() -> None:
    session = FakeSession()
    worker_config = config()
    await session.handshake(worker_config)
    await session.register(["alpha", "beta"], ["alpha", "beta"])
    assert session.handshakes == [("queue-a", "worker-a")]
    assert session.registrations == [["alpha", "beta"]]


async def test_serve_applies_max_concurrency_backpressure() -> None:
    session = FakeSession()
    release = asyncio.Event()
    dispatcher = RecordingDispatcher(release)
    for index in range(5):
        await session.push(task(index + 1))
    await session.finish()

    serving = asyncio.create_task(serve(config(max_concurrency=2), session, dispatcher))
    while dispatcher.started < 2:
        await asyncio.sleep(0)
    await asyncio.sleep(0.02)
    assert dispatcher.started == 2
    assert dispatcher.peak == 2

    release.set()
    await serving
    assert dispatcher.started == 5
    assert dispatcher.peak == 2


async def test_reconnect_re_reports_unacked_before_dispatching_new_task() -> None:
    second = FakeSession()
    tracker = UnackedResultTracker()
    dispatcher = RecordingDispatcher()
    tracker.record(
        aion_worker.PendingCompletedReport(
            workflow_id=workflow_id(),
            activity_id=activity_id(7),
            output=payload(),
        )
    )
    await second.push(task(8))
    await second.finish()
    attempts = 0

    async def fail_then_connect() -> FakeSession:
        nonlocal attempts
        attempts += 1
        if attempts == 1:
            raise OSError("dropped")
        return second

    await connect_register_replay_and_serve(config(), fail_then_connect, dispatcher, tracker)
    assert second.log[:4] == ["handshake", "register", "result:7", "receive"]
    assert dispatcher.dispatched == [8]
