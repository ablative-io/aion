from __future__ import annotations

import asyncio
from collections.abc import Iterable
from typing import Any, NoReturn, cast

import grpc
import pytest
from ar007_fakes import (
    FakeGrpcStream,
    FakeSession,
    RecordingDispatcher,
    activity_id,
    config,
    payload,
    register_ack_frame,
    task,
    wait_for_condition,
    workflow_id,
)

import aion_worker
from aion_worker import (
    ReconnectBackoff,
    ReconnectConfig,
    ReconnectError,
    TransportCredentials,
    TransportError,
    UnackedResultTracker,
    WorkerConfig,
    connect_register_replay_and_serve,
    reconnect_with_backoff,
    serve,
)
from aion_worker.proto import common_pb2, worker_pb2


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
    await wait_for_condition(serving, lambda: dispatcher.started >= 2)
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
    attempts = 0

    async def fail_then_connect() -> FakeSession:
        nonlocal attempts
        attempts += 1
        if attempts == 1:
            raise OSError("dropped")
        return second

    # A clean close no longer ends the run, so the loop is driven to a
    # graceful shutdown once the new task has been dispatched.
    shutdown = asyncio.Event()
    run = asyncio.create_task(
        connect_register_replay_and_serve(config(), fail_then_connect, dispatcher, tracker, shutdown=shutdown)
    )
    await wait_for_condition(run, lambda: dispatcher.dispatched == [8])
    shutdown.set()
    await run

    assert second.log[:4] == ["handshake", "register", "result:7", "receive"]
    assert dispatcher.dispatched == [8]


def test_tracker_keeps_reports_for_distinct_workflows_at_same_sequence_position() -> None:
    first_workflow = common_pb2.WorkflowId(uuid="workflow-a")
    second_workflow = common_pb2.WorkflowId(uuid="workflow-b")
    tracker = UnackedResultTracker()

    tracker.record(
        aion_worker.PendingCompletedReport(
            workflow_id=first_workflow,
            activity_id=activity_id(3),
            output=payload(),
        )
    )
    tracker.record(
        aion_worker.PendingCompletedReport(
            workflow_id=second_workflow,
            activity_id=activity_id(3),
            output=payload(),
        )
    )

    assert len(tracker) == 2
    assert tracker.get(first_workflow, activity_id(3)) is not None
    assert tracker.get(second_workflow, activity_id(3)) is not None
    tracker.acknowledge(first_workflow, activity_id(3))
    assert len(tracker) == 1
    assert tracker.get(first_workflow, activity_id(3)) is None
    assert tracker.get(second_workflow, activity_id(3)) is not None


class WorkflowResultSession(FakeSession):
    """FakeSession that logs the owning workflow with each result report."""

    async def report_result(
        self,
        workflow: common_pb2.WorkflowId,
        activity: common_pb2.ActivityId,
        result: common_pb2.Payload,
    ) -> None:
        self.log.append(f"result:{workflow.uuid}:{activity.sequence_position}")


def _workflow_task(workflow_uuid: str, sequence_position: int) -> aion_worker.TaskReceived:
    return aion_worker.TaskReceived(
        aion_worker.ActivityTask(
            workflow_id=common_pb2.WorkflowId(uuid=workflow_uuid),
            activity_id=activity_id(sequence_position),
            activity_type="slow",
            input=payload(),
            attempt=1,
        )
    )


async def test_reconnect_re_reports_unacked_for_all_workflows_with_colliding_positions() -> None:
    first = WorkflowResultSession(drop_after_events=True)
    second = WorkflowResultSession()
    dispatcher = RecordingDispatcher()
    await first.push(_workflow_task("workflow-a", 3))
    await first.push(_workflow_task("workflow-b", 3))
    await first.finish()
    sessions = [first, second]

    async def connect() -> FakeSession:
        return sessions.pop(0)

    async def no_sleep(delay: float) -> None:
        del delay

    # The second session stays open (no clean close), and the run is ended by
    # a graceful shutdown once both replays have been observed.
    shutdown = asyncio.Event()
    run = asyncio.create_task(
        connect_register_replay_and_serve(config(), connect, dispatcher, sleep=no_sleep, shutdown=shutdown)
    )
    await wait_for_condition(
        run,
        lambda: len([entry for entry in second.log if entry.startswith("result:")]) >= 2,
    )
    shutdown.set()
    await run

    replayed = [entry for entry in second.log if entry.startswith("result:")]
    assert sorted(replayed) == ["result:workflow-a:3", "result:workflow-b:3"], (
        "both workflows' colliding sequence-position results must be re-reported"
    )


async def test_reconnect_after_stream_drop_uses_backoff_and_replays_unacked(
    caplog: pytest.LogCaptureFixture,
) -> None:
    first = FakeSession(drop_after_events=True)
    second = FakeSession()
    dispatcher = RecordingDispatcher()
    await first.push(task(7))
    await first.finish()
    await second.push(task(8))
    sessions = [first, second]
    sleeps: list[float] = []

    async def connect() -> FakeSession:
        return sessions.pop(0)

    async def record_sleep(delay: float) -> None:
        sleeps.append(delay)

    # The second session stays open (no clean close); the run ends by
    # graceful shutdown after both tasks have been dispatched.
    shutdown = asyncio.Event()
    with caplog.at_level("WARNING"):
        run = asyncio.create_task(
            connect_register_replay_and_serve(config(), connect, dispatcher, sleep=record_sleep, shutdown=shutdown)
        )
        await wait_for_condition(run, lambda: dispatcher.dispatched == [7, 8])
        shutdown.set()
        await run

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


async def test_shutdown_during_establishment_backoff_returns_promptly() -> None:
    """Shutdown is raced against the establishment-backoff sleep, never waited out.

    Parity with the Rust worker's
    ``shutdown_during_establishment_backoff_returns_promptly``: a worker stuck
    in establishment retries (server down, long delays) must honour SIGTERM
    immediately instead of stalling through the remaining backoff schedule.
    """

    shutdown = asyncio.Event()
    backoff_entered = asyncio.Event()
    attempts = 0

    async def fail_connect() -> NoReturn:
        nonlocal attempts
        attempts += 1
        raise OSError("dial refused")

    async def hanging_sleep(delay: float) -> None:
        del delay
        backoff_entered.set()
        # Stands in for an arbitrarily long backoff: it never completes, so
        # only the shutdown race can end the wait.
        await asyncio.Event().wait()

    run = asyncio.create_task(
        connect_register_replay_and_serve(
            config(), fail_connect, RecordingDispatcher(), sleep=hanging_sleep, shutdown=shutdown
        )
    )
    await asyncio.wait_for(backoff_entered.wait(), timeout=5)
    shutdown.set()
    # Returns cleanly well before any backoff could elapse — the timeout only
    # fires if the worker stalls waiting out the establishment sleep.
    await asyncio.wait_for(run, timeout=5)

    # Exactly the one pre-shutdown dial: shutdown never grows the session count.
    assert attempts == 1


async def test_already_set_shutdown_never_dials() -> None:
    """Direct reconnect_with_backoff contract: a pre-set shutdown event short-circuits.

    Parity with the TypeScript worker's "resolves undefined without dialling
    when the signal is already aborted": the function returns ``None`` before
    any dial attempt, so a shutdown request that predates establishment never
    opens a session it would immediately have to close.
    """

    shutdown = asyncio.Event()
    shutdown.set()
    attempts = 0

    async def connect() -> FakeSession:
        nonlocal attempts
        attempts += 1
        return FakeSession()

    session = await reconnect_with_backoff(
        connect=connect,
        config=config(),
        activity_types=["greet"],
        available_handlers=["greet"],
        backoff=ReconnectBackoff.from_config(config()),
        shutdown=shutdown,
    )

    assert session is None
    assert attempts == 0


async def test_shutdown_during_hung_dial_cancels_the_attempt_and_returns_promptly() -> None:
    """Shutdown is raced against the WHOLE establishment attempt, not just the sleeps.

    Parity with the Rust worker, which selects shutdown around the entire
    establishment in ``run_with_connector_until``: a SIGTERM during a hung
    connect must return promptly (and cancel the in-flight dial) instead of
    waiting out the transport's own connect behaviour.
    """

    shutdown = asyncio.Event()
    dial_entered = asyncio.Event()
    dial_cancelled = asyncio.Event()
    attempts = 0

    async def hung_connect() -> NoReturn:
        nonlocal attempts
        attempts += 1
        dial_entered.set()
        try:
            # Stands in for a hung connect: it never completes, so only the
            # shutdown race (via cancellation) can end the attempt.
            await asyncio.Event().wait()
        except asyncio.CancelledError:
            dial_cancelled.set()
            raise
        raise AssertionError("the hung dial can only end by cancellation")

    run = asyncio.create_task(
        reconnect_with_backoff(
            connect=hung_connect,
            config=config(),
            activity_types=["greet"],
            available_handlers=["greet"],
            backoff=ReconnectBackoff.from_config(config()),
            shutdown=shutdown,
        )
    )
    await asyncio.wait_for(dial_entered.wait(), timeout=5)
    shutdown.set()
    # Returns None well before any transport timeout could elapse — the
    # wait_for only fires if shutdown failed to win against the hung dial.
    session = await asyncio.wait_for(run, timeout=5)

    assert session is None
    assert attempts == 1
    assert dial_cancelled.is_set()


class HangingRegisterSession(FakeSession):
    """Session whose registration hangs until the attempt is cancelled."""

    def __init__(self, entered: asyncio.Event) -> None:
        super().__init__()
        self.register_entered = entered

    async def register(self, activity_types: Iterable[str], available_handlers: Iterable[str]) -> None:
        del activity_types, available_handlers
        self.register_entered.set()
        await asyncio.Event().wait()


async def test_shutdown_during_hung_register_closes_the_partial_session() -> None:
    """Cancellation mid-register never leaks the channel.

    When shutdown cancels an attempt that already dialled, the attempt closes
    its partially-established session while unwinding — the close runs inside
    the cancelled task before the cancellation propagates.
    """

    shutdown = asyncio.Event()
    register_entered = asyncio.Event()
    session = HangingRegisterSession(register_entered)

    async def connect() -> FakeSession:
        return session

    run = asyncio.create_task(
        reconnect_with_backoff(
            connect=connect,
            config=config(),
            activity_types=["greet"],
            available_handlers=["greet"],
            backoff=ReconnectBackoff.from_config(config()),
            shutdown=shutdown,
        )
    )
    await asyncio.wait_for(register_entered.wait(), timeout=5)
    shutdown.set()
    result = await asyncio.wait_for(run, timeout=5)

    assert result is None
    assert session.closed is True
    assert session.log == ["handshake", "close"]


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

    # The session stays open (no clean close); the run ends by graceful
    # shutdown after the completion log line has been observed.
    shutdown = asyncio.Event()
    with caplog.at_level("INFO"):
        run = asyncio.create_task(
            connect_register_replay_and_serve(worker_config, connect, dispatcher, shutdown=shutdown)
        )
        await wait_for_condition(
            run,
            lambda: "Completed greet in 5ms" in [record.getMessage() for record in caplog.records],
        )
        shutdown.set()
        await run

    messages = [record.getMessage() for record in caplog.records]
    assert "Connected successfully" not in messages
    assert "Connected" in messages
    assert messages.index("Connected") < messages.index("Registered activities: greet")
    assert session.log[:2] == ["handshake", "register"]
    assert "Registered activities: greet" in messages
    assert "Waiting for tasks" in messages
    assert "Received task greet for workflow workflow-1 (attempt 1)" in messages
    assert "Completed greet in 5ms" in messages


def _denial(code: grpc.StatusCode, details: str) -> grpc.aio.AioRpcError:
    return grpc.aio.AioRpcError(code, grpc.aio.Metadata(), grpc.aio.Metadata(), details=details)


class DeniedRegisterSession(FakeSession):
    """Session whose registration the server rejects with a gRPC denial."""

    def __init__(self, denial: grpc.aio.AioRpcError) -> None:
        super().__init__()
        self.denial = denial

    async def register(self, activity_types: Iterable[str], available_handlers: Iterable[str]) -> None:
        del activity_types, available_handlers
        self.log.append("register")
        raise TransportError(f"worker stream connection failed: {self.denial}") from self.denial


async def test_permission_denied_connect_stops_after_one_attempt() -> None:
    attempts = 0
    sleeps: list[float] = []

    async def denied_connect() -> NoReturn:
        nonlocal attempts
        attempts += 1
        raise _denial(grpc.StatusCode.PERMISSION_DENIED, "namespace 'payments' is not granted")

    async def record_sleep(delay: float) -> None:
        sleeps.append(delay)

    with pytest.raises(grpc.aio.AioRpcError) as denied:
        await connect_register_replay_and_serve(config(), denied_connect, RecordingDispatcher(), sleep=record_sleep)

    assert attempts == 1
    assert sleeps == []
    assert denied.value.code() is grpc.StatusCode.PERMISSION_DENIED
    assert denied.value.details() == "namespace 'payments' is not granted"


async def test_unauthenticated_register_stops_after_one_attempt_and_closes_session() -> None:
    denial = _denial(grpc.StatusCode.UNAUTHENTICATED, "worker credentials were rejected")
    session = DeniedRegisterSession(denial)
    attempts = 0
    sleeps: list[float] = []

    async def connect() -> DeniedRegisterSession:
        nonlocal attempts
        attempts += 1
        return session

    async def record_sleep(delay: float) -> None:
        sleeps.append(delay)

    with pytest.raises(TransportError) as denied:
        await connect_register_replay_and_serve(config(), connect, RecordingDispatcher(), sleep=record_sleep)

    assert attempts == 1
    assert sleeps == []
    assert session.closed is True
    assert denied.value.__cause__ is denial
    assert "worker credentials were rejected" in str(denied.value)
    assert aion_worker.grpc_status_code(denied.value) is grpc.StatusCode.UNAUTHENTICATED


async def test_unavailable_connect_still_retries_until_attempts_exhausted() -> None:
    attempts = 0
    sleeps: list[float] = []

    async def unavailable_connect() -> NoReturn:
        nonlocal attempts
        attempts += 1
        raise _denial(grpc.StatusCode.UNAVAILABLE, "engine unreachable")

    async def record_sleep(delay: float) -> None:
        sleeps.append(delay)

    worker_config = config()
    with pytest.raises(ReconnectError, match=worker_config.endpoint):
        await connect_register_replay_and_serve(
            worker_config, unavailable_connect, RecordingDispatcher(), sleep=record_sleep
        )

    assert attempts == 3
    assert sleeps == [0.01, 0.02]


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


async def test_grpc_register_succeeds_only_on_register_ack_and_captures_payload() -> None:
    """Brief test 23: success requires the RegisterAck frame, not readiness."""

    worker_config = config()
    session = aion_worker.GrpcWorkerSession(worker_config)
    stream = FakeGrpcStream(frames=[register_ack_frame(worker_id=9, heartbeat_window_ms=12_000)])
    session._outbound = asyncio.Queue(maxsize=16)
    session._stream = stream

    await session.register(["slow"], ["slow"])

    assert stream.reads == 1
    info = session.registered_info
    assert info is not None
    assert info.worker_id == 9
    assert info.namespace == "queue-a"
    assert info.heartbeat_window_ms == 12_000
    outbound = session._outbound
    assert outbound is not None
    message = await outbound.get()
    assert message is not None
    assert message.register.activity_types == ["slow"]


async def test_grpc_register_times_out_retryably_when_server_never_acks() -> None:
    """Brief test 23: the ack wait is bounded by max_backoff_seconds."""

    worker_config = config()
    session = aion_worker.GrpcWorkerSession(worker_config)
    session._outbound = asyncio.Queue(maxsize=16)
    session._stream = FakeGrpcStream(hang_after_frames=True)

    with pytest.raises(aion_worker.TransportError, match="did not acknowledge registration") as failure:
        await session.register(["slow"], ["slow"])
    assert aion_worker.is_retryable_session_error(failure.value)


async def test_grpc_register_rejects_non_ack_first_frame_as_protocol_violation() -> None:
    worker_config = config()
    session = aion_worker.GrpcWorkerSession(worker_config)
    session._outbound = asyncio.Queue(maxsize=16)
    session._stream = FakeGrpcStream(
        frames=[
            worker_pb2.ServerToWorker(
                task=worker_pb2.ActivityTask(activity_type="slow", attempt=1),
            )
        ]
    )

    with pytest.raises(aion_worker.TransportError, match="protocol violation"):
        await session.register(["slow"], ["slow"])


async def test_grpc_register_surfaces_stream_end_before_ack_as_retryable() -> None:
    worker_config = config()
    session = aion_worker.GrpcWorkerSession(worker_config)
    session._outbound = asyncio.Queue(maxsize=16)
    session._stream = FakeGrpcStream()

    with pytest.raises(aion_worker.TransportError, match="ended the stream before acknowledging") as failure:
        await session.register(["slow"], ["slow"])
    assert aion_worker.is_retryable_session_error(failure.value)


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

    expected_message = f"Connection failed to {worker_config.endpoint}: dial refused"
    failure_records = [record for record in caplog.records if record.getMessage() == expected_message]
    assert failure_records
    assert all(record.levelname == "ERROR" for record in failure_records)


# --- Worker-protocol ack wave: decode taxonomy + attempt validation --------


def test_decode_server_message_maps_drain_to_drain_received() -> None:
    """Brief test 24: the drain frame is a first-class event — the old
    TransportError("unsupported ... 'drain'") path is the bug being killed."""

    from aion_worker import DrainReceived
    from aion_worker.session import decode_server_message

    event = decode_server_message(worker_pb2.ServerToWorker(drain=worker_pb2.DrainRequest()))
    assert isinstance(event, DrainReceived)


def test_decode_server_message_maps_result_ack() -> None:
    from aion_worker import ResultAcknowledged
    from aion_worker.session import decode_server_message

    event = decode_server_message(
        worker_pb2.ServerToWorker(
            result_ack=worker_pb2.ResultAck(
                workflow_id=common_pb2.WorkflowId(uuid="workflow-1"),
                activity_id=common_pb2.ActivityId(sequence_position=4),
            )
        )
    )
    assert isinstance(event, ResultAcknowledged)
    assert event.workflow_id.uuid == "workflow-1"
    assert event.activity_id.sequence_position == 4


def test_decode_server_message_rejects_mid_stream_register_ack() -> None:
    from aion_worker.session import decode_server_message

    with pytest.raises(TransportError, match="protocol violation"):
        decode_server_message(worker_pb2.ServerToWorker(register_ack=worker_pb2.RegisterAck(worker_id=1)))


def test_decode_server_message_still_rejects_empty_payload() -> None:
    from aion_worker.session import decode_server_message

    with pytest.raises(TransportError, match="no payload set"):
        decode_server_message(worker_pb2.ServerToWorker())


def test_activity_task_reads_wire_attempt_and_rejects_zero() -> None:
    """Brief test 26: the context attempt comes from the wire; zero is malformed."""

    stamped = aion_worker.ActivityTask.from_proto(
        worker_pb2.ActivityTask(
            workflow_id=common_pb2.WorkflowId(uuid="workflow-1"),
            activity_id=common_pb2.ActivityId(sequence_position=1),
            activity_type="slow",
            input=common_pb2.Payload(content_type="application/json", bytes=b"{}"),
            attempt=3,
        )
    )
    assert stamped.attempt == 3

    with pytest.raises(TransportError, match="attempt is missing or zero"):
        aion_worker.ActivityTask.from_proto(
            worker_pb2.ActivityTask(
                workflow_id=common_pb2.WorkflowId(uuid="workflow-1"),
                activity_id=common_pb2.ActivityId(sequence_position=1),
                activity_type="slow",
                input=common_pb2.Payload(content_type="application/json", bytes=b"{}"),
            )
        )


def test_wire_default_attempt_constant_is_gone() -> None:
    """Brief test 26: the consumer-side parity hack is deleted outright."""

    import aion_worker.context as context_module

    assert not hasattr(context_module, "WIRE_DEFAULT_ATTEMPT")
    assert "WIRE_DEFAULT_ATTEMPT" not in aion_worker.__all__


async def test_wire_attempt_is_exposed_on_the_activity_context() -> None:
    """Brief test 26 (loop half): the handler context exposes the wire attempt."""

    from aion_worker import ActivityExecutionContext, Completed, DispatchOutcome, TaskReceived, serve

    seen: list[int] = []

    class AttemptRecordingDispatcher:
        def activity_types(self) -> Iterable[str]:
            return ["slow"]

        async def dispatch(
            self, dispatched: aion_worker.ActivityTask, context: ActivityExecutionContext
        ) -> DispatchOutcome:
            seen.append(context.attempt)
            return Completed(payload())

    session = FakeSession()
    await session.push(
        TaskReceived(
            aion_worker.ActivityTask(
                workflow_id=workflow_id(),
                activity_id=activity_id(1),
                activity_type="slow",
                input=payload(),
                attempt=5,
            )
        )
    )
    await session.finish()

    await serve(config(), session, AttemptRecordingDispatcher())

    assert seen == [5]
