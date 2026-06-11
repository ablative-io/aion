from __future__ import annotations

import asyncio
from collections.abc import AsyncIterator, Callable, Iterable
from dataclasses import dataclass, field
from typing import TypeVar

from aion_worker import (
    ActivityExecutionContext,
    ActivityTask,
    Completed,
    DispatchOutcome,
    ReconnectConfig,
    TaskReceived,
    WorkerConfig,
    WorkerSessionEvent,
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


class FakeGrpcStream:
    def __init__(self, failure: Exception | None = None) -> None:
        self.failure = failure
        self.waited = False

    async def wait_for_connection(self) -> None:
        self.waited = True
        if self.failure is not None:
            raise self.failure

    def __aiter__(self) -> FakeGrpcStream:
        return self

    async def __anext__(self) -> worker_pb2.ServerToWorker:
        raise StopAsyncIteration


@dataclass
class FakeSession:
    events: asyncio.Queue[WorkerSessionEvent | None] = field(default_factory=asyncio.Queue)
    log: list[str] = field(default_factory=list)
    handshakes: list[tuple[str, str]] = field(default_factory=list)
    registrations: list[list[str]] = field(default_factory=list)
    drop_after_events: bool = False
    closed: bool = False

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

    async def close(self) -> None:
        self.closed = True
        self.log.append("close")

    def receive_tasks(self) -> AsyncIterator[WorkerSessionEvent]:
        return self._receive()

    async def _receive(self) -> AsyncIterator[WorkerSessionEvent]:
        while True:
            event = await self.events.get()
            if event is None:
                if self.drop_after_events:
                    raise OSError("stream dropped")
                return
            self.log.append("receive")
            yield event


class RecordingDispatcher:
    def __init__(self, release: asyncio.Event | None = None, activity_type: str = "slow") -> None:
        self.active = 0
        self.peak = 0
        self.started = 0
        self.release = release
        self.activity_type = activity_type
        self.dispatched: list[int] = []

    def activity_types(self) -> Iterable[str]:
        return [self.activity_type]

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


_TaskResult = TypeVar("_TaskResult")


async def wait_for_condition(
    run: asyncio.Task[_TaskResult],
    condition: Callable[[], bool],
    timeout_seconds: float = 5.0,
) -> None:
    """Spin (cooperatively) until ``condition`` holds, with guarded failure.

    Fails clearly — instead of hanging the suite forever — when the worker
    run task ends before the condition is met (surfacing the run's own
    exception when it failed) or when the wall-clock deadline passes.
    """

    loop = asyncio.get_running_loop()
    deadline = loop.time() + timeout_seconds
    while not condition():
        if run.done():
            run.result()
            raise AssertionError("worker run ended before the awaited test condition was met")
        if loop.time() >= deadline:
            raise AssertionError("timed out waiting for the test condition")
        await asyncio.sleep(0)


def task(sequence_position: int, activity_type: str = "slow") -> TaskReceived:
    return TaskReceived(
        ActivityTask(
            workflow_id=workflow_id(),
            activity_id=activity_id(sequence_position),
            activity_type=activity_type,
            input=payload(),
        )
    )
