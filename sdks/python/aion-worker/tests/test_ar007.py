from __future__ import annotations

import asyncio
from typing import Any, NoReturn, cast

import pytest

import aion_worker
from aion_worker import (
    ReconnectConfig,
    ReconnectError,
    TransportCredentials,
    UnackedResultTracker,
    WorkerConfig,
    connect_register_replay_and_serve,
    serve,
)

from ar007_fakes import (
    FakeGrpcStream,
    FakeSession,
    RecordingDispatcher,
    activity_id,
    config,
    payload,
    task,
    workflow_id,
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


async def test_reconnect_after_stream_drop_uses_backoff_and_replays_unacked(
    caplog: pytest.LogCaptureFixture,
) -> None:
    first = FakeSession(drop_after_events=True)
    second = FakeSession()
    dispatcher = RecordingDispatcher()
    await first.push(task(7))
    await first.finish()
    await second.push(task(8))
    await second.finish()
    sessions = [first, second]
    sleeps: list[float] = []

    async def connect() -> FakeSession:
        return sessions.pop(0)

    async def record_sleep(delay: float) -> None:
        sleeps.append(delay)

    with caplog.at_level("WARNING"):
        await connect_register_replay_and_serve(config(), connect, dispatcher, sleep=record_sleep)

    assert first.closed is True
    assert sleeps == [0.01]
    assert first.log[-1] == "close"
    assert second.log[:3] == ["handshake", "register", "result:7"]
    assert dispatcher.dispatched == [7, 8]


async def test_reconnect_logs_attempts_delays_and_exhausts_cleanly(
    caplog: pytest.LogCaptureFixture,
) -> None:
    sleeps: list[float] = []
    attempts = 0

    async def fail_connect() -> NoReturn:
        nonlocal attempts
        attempts += 1
        raise OSError("dial refused")

    async def record_sleep(delay: float) -> None:
        sleeps.append(delay)

    worker_config = config()
    with caplog.at_level("WARNING"), pytest.raises(ReconnectError, match=worker_config.endpoint):
        await connect_register_replay_and_serve(worker_config, fail_connect, RecordingDispatcher(), sleep=record_sleep)

    messages = [record.getMessage() for record in caplog.records]
    assert "Connected" not in messages
    assert "Connected successfully" not in messages
    assert "Reconnecting in 0.01s (attempt 1/3)" in messages
    assert "Reconnecting in 0.02s (attempt 2/3)" in messages
    assert sleeps == [0.01, 0.02]
    assert attempts == 3


async def test_worker_lifecycle_logs_startup_registration_waiting_receipt_and_completion(
    caplog: pytest.LogCaptureFixture, monkeypatch: pytest.MonkeyPatch
) -> None:
    session = FakeSession()
    dispatcher = RecordingDispatcher(activity_type="greet")
    perf_ticks = iter([10.0, 10.005])
    monkeypatch.setattr("aion_worker.loop.time.perf_counter", lambda: next(perf_ticks))
    worker_config = WorkerConfig(
        endpoint="127.0.0.1:50051",
        task_queue="queue-a",
        identity="worker-a",
        max_concurrency=2,
        reconnect=ReconnectConfig(initial_backoff_seconds=0.01, max_backoff_seconds=0.02, max_attempts=3),
    )

    async def connect() -> FakeSession:
        return session

    await session.push(task(1, "greet"))
    await session.finish()

    with caplog.at_level("INFO"):
        await connect_register_replay_and_serve(worker_config, connect, dispatcher)

    messages = [record.getMessage() for record in caplog.records]
    assert "Connected successfully" not in messages
    assert "Connected" in messages
    assert messages.index("Connected") < messages.index("Registered activities: greet")
    assert session.log[:2] == ["handshake", "register"]
    assert "Registered activities: greet" in messages
    assert "Waiting for tasks" in messages
    assert "Received task greet for workflow workflow-1" in messages
    assert "Completed greet in 5ms" in messages


async def test_worker_config_grpc_metadata_includes_namespace_and_subject() -> None:
    default_config = config()
    assert default_config.grpc_metadata() == (
        ("x-aion-namespaces", "default"),
        ("x-aion-subject", "worker"),
    )

    configured = WorkerConfig(
        endpoint="127.0.0.1:50051",
        task_queue="queue-a",
        identity="worker-a",
        max_concurrency=2,
        reconnect=ReconnectConfig(
            initial_backoff_seconds=0.01,
            max_backoff_seconds=0.02,
            max_attempts=3,
        ),
        transport_credentials=TransportCredentials(
            metadata=(
                ("authorization", "Bearer token"),
                ("x-aion-namespaces", "stale"),
                ("X-Aion-Subject", "stale"),
            )
        ),
        namespace="payments",
        subject="worker-a",
    )

    assert configured.grpc_metadata() == (
        ("authorization", "Bearer token"),
        ("x-aion-namespaces", "payments"),
        ("x-aion-subject", "worker-a"),
    )


class RecordingStreamWorker:
    def __init__(self) -> None:
        self.metadata: tuple[tuple[str, str], ...] | None = None

    def __call__(
        self,
        request_iterator: object,
        *,
        metadata: tuple[tuple[str, str], ...],
    ) -> FakeGrpcStream:
        del request_iterator
        self.metadata = metadata
        return FakeGrpcStream()


class RecordingGrpcStub:
    def __init__(self) -> None:
        self.stream_worker = RecordingStreamWorker()

    @property
    def StreamWorker(self) -> RecordingStreamWorker:
        return self.stream_worker


async def test_grpc_open_stream_sends_authorization_metadata() -> None:
    worker_config = config()
    session = aion_worker.GrpcWorkerSession(worker_config)
    stub = RecordingGrpcStub()
    session._stub = cast(Any, stub)

    await session._open_stream()

    assert stub.StreamWorker.metadata == (
        ("x-aion-namespaces", "default"),
        ("x-aion-subject", "worker"),
    )


async def test_grpc_register_waits_for_transport_connection_before_success() -> None:
    worker_config = config()
    session = aion_worker.GrpcWorkerSession(worker_config)
    stream = FakeGrpcStream()
    session._outbound = asyncio.Queue(maxsize=16)
    session._stream = stream

    await session.register(["slow"], ["slow"])

    assert stream.waited is True
    outbound = session._outbound
    assert outbound is not None
    message = await outbound.get()
    assert message is not None
    assert message.register.activity_types == ["slow"]


async def test_grpc_register_surfaces_transport_connection_failure() -> None:
    worker_config = config()
    session = aion_worker.GrpcWorkerSession(worker_config)
    session._outbound = asyncio.Queue(maxsize=16)
    session._stream = FakeGrpcStream(OSError("dial refused"))

    with pytest.raises(aion_worker.TransportError, match="dial refused"):
        await session.register(["slow"], ["slow"])


async def test_grpc_connect_logs_configured_endpoint(caplog: pytest.LogCaptureFixture) -> None:
    worker_config = WorkerConfig(
        endpoint="127.0.0.1:50051",
        task_queue="queue-a",
        identity="worker-a",
        max_concurrency=2,
        reconnect=ReconnectConfig(initial_backoff_seconds=0.01, max_backoff_seconds=0.02, max_attempts=3),
    )

    with caplog.at_level("INFO"):
        session = await aion_worker.GrpcWorkerSession.connect(worker_config)
    await session.close()

    assert "Connecting to 127.0.0.1:50051" in [record.getMessage() for record in caplog.records]


async def test_reconnect_logs_connection_failure_reason(caplog: pytest.LogCaptureFixture) -> None:
    async def fail_connect() -> NoReturn:
        raise OSError("dial refused")

    worker_config = config()
    with caplog.at_level("ERROR"), pytest.raises(ReconnectError, match="dial refused"):
        await connect_register_replay_and_serve(worker_config, fail_connect, RecordingDispatcher())

    failure_records = [record for record in caplog.records if record.getMessage() == f"Connection failed to {worker_config.endpoint}: dial refused"]
    assert failure_records
    assert all(record.levelname == "ERROR" for record in failure_records)
