"""#176 automatic liveness pump: the RUNTIME heartbeats in-flight activities.

The server's heartbeat sweeper expires any worker whose in-flight task goes a
full heartbeat window without a heartbeat. That is dead/wedged-process
detection — a healthy worker running a legitimately long activity whose
handler never calls ``ActivityContext.heartbeat`` must never trip it, so the
serve loop pumps automatic liveness beats for every in-flight activity at a
quarter-window cadence, derived from the session's server-assigned window
(``RegisterAck.heartbeat_window_ms``).
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass, field

from ar007_fakes import FakeSession, RecordingDispatcher, config, task

from aion_worker import serve
from aion_worker.proto import common_pb2


@dataclass
class WindowFakeSession(FakeSession):
    """Fake session that advertises a server-assigned heartbeat window."""

    window_seconds: float | None = None
    heartbeats: list[tuple[str, int, common_pb2.Payload | None]] = field(default_factory=list)

    def heartbeat_window_seconds(self) -> float | None:
        return self.window_seconds

    async def send_heartbeat(
        self,
        workflow: common_pb2.WorkflowId,
        activity: common_pb2.ActivityId,
        progress: common_pb2.Payload | None,
    ) -> None:
        self.heartbeats.append((workflow.uuid, activity.sequence_position, progress))
        await super().send_heartbeat(workflow, activity, progress)


async def test_runtime_auto_heartbeats_long_activity_within_the_window() -> None:
    """A handler that outlives several windows is beaten automatically, with no progress payload."""

    window = 0.1
    session = WindowFakeSession(window_seconds=window)
    release = asyncio.Event()
    dispatcher = RecordingDispatcher(release)
    await session.push(task(7))

    serving = asyncio.create_task(serve(config(max_concurrency=1), session, dispatcher))
    # Three beats prove at least one full window elapsed with sub-window
    # beats (the pump cadence is a quarter window).
    deadline = asyncio.get_running_loop().time() + 10.0
    while len(session.heartbeats) < 3:
        assert not serving.done(), "serve ended before the pump was observed"
        assert asyncio.get_running_loop().time() < deadline, "runtime never auto-heartbeated the in-flight activity"
        await asyncio.sleep(0.005)
    assert all(
        uuid == "workflow-1" and sequence_position == 7 and progress is None
        for (uuid, sequence_position, progress) in session.heartbeats
    ), f"automatic beats must target the in-flight task with no progress: {session.heartbeats}"

    # Once the handler completes, the pump must stop beating.
    release.set()
    await asyncio.sleep(window)
    settled = len(session.heartbeats)
    await asyncio.sleep(window * 2)
    assert len(session.heartbeats) == settled, "the pump must stop once nothing is in flight"

    await session.finish()
    await serving
    assert f"result:{7}" in session.log


async def test_no_window_means_no_automatic_heartbeats() -> None:
    """A session without a server-assigned window (every fake) never pumps."""

    session = WindowFakeSession(window_seconds=None)
    release = asyncio.Event()
    dispatcher = RecordingDispatcher(release)
    await session.push(task(8))

    serving = asyncio.create_task(serve(config(max_concurrency=1), session, dispatcher))
    # Long enough that a (wrongly) armed pump at any plausible cadence would
    # have beaten several times.
    await asyncio.sleep(0.1)
    assert session.heartbeats == []

    release.set()
    await session.finish()
    await serving
