"""Backoff reconnect, re-register, and unacknowledged result replay."""

from __future__ import annotations

import asyncio
from collections import OrderedDict
from collections.abc import Awaitable, Callable, Iterable
from dataclasses import dataclass
from typing import TypeAlias

from .session import ActivityError, ActivityId, Payload, WorkerConfig, WorkerSession, WorkflowId

ActivitySequence: TypeAlias = int
ConnectFactory: TypeAlias = Callable[[], Awaitable[WorkerSession]]
SleepFactory: TypeAlias = Callable[[float], Awaitable[None]]


class ReconnectError(Exception):
    """Raised when reconnect policy is invalid or exhausted."""


@dataclass(frozen=True)
class PendingCompletedReport:
    """Completion report computed locally but not acknowledged by the engine."""

    workflow_id: WorkflowId
    activity_id: ActivityId
    output: Payload


@dataclass(frozen=True)
class PendingFailedReport:
    """Failure report computed locally but not acknowledged by the engine."""

    workflow_id: WorkflowId
    activity_id: ActivityId
    failure: ActivityError


PendingActivityReport: TypeAlias = PendingCompletedReport | PendingFailedReport


class UnackedResultTracker:
    """Tracks reported activity outcomes until AW adds an explicit ack frame."""

    def __init__(self) -> None:
        self._pending: OrderedDict[ActivitySequence, PendingActivityReport] = OrderedDict()

    def record(self, report: PendingActivityReport) -> None:
        self._pending[activity_sequence(report.activity_id)] = report

    def acknowledge(self, activity_id: ActivityId) -> None:
        self._pending.pop(activity_sequence(activity_id), None)

    def get(self, activity_id: ActivityId) -> PendingActivityReport | None:
        return self._pending.get(activity_sequence(activity_id))

    def snapshot(self) -> tuple[PendingActivityReport, ...]:
        return tuple(self._pending.values())

    def __len__(self) -> int:
        return len(self._pending)

    def is_empty(self) -> bool:
        return not self._pending


@dataclass(frozen=True)
class ReconnectBackoff:
    """Validated bounded exponential reconnect policy."""

    initial_backoff_seconds: float
    max_backoff_seconds: float
    max_attempts: int

    @classmethod
    def from_config(cls, config: WorkerConfig) -> ReconnectBackoff:
        reconnect = config.reconnect
        policy = cls(
            initial_backoff_seconds=reconnect.initial_backoff_seconds,
            max_backoff_seconds=reconnect.max_backoff_seconds,
            max_attempts=reconnect.max_attempts,
        )
        policy.validate()
        return policy

    def validate(self) -> None:
        if self.initial_backoff_seconds <= 0:
            raise ReconnectError("initial reconnect backoff must be greater than zero")
        if self.max_backoff_seconds <= 0:
            raise ReconnectError("max reconnect backoff must be greater than zero")
        if self.max_attempts <= 0:
            raise ReconnectError("max reconnect attempts must be greater than zero")

    def delay_for_attempt(self, attempt: int) -> float:
        if attempt <= 0:
            raise ReconnectError("reconnect attempt must be greater than zero")
        delay = self.initial_backoff_seconds * float(2 ** (attempt - 1))
        return min(delay, self.max_backoff_seconds)


async def reconnect_with_backoff(
    connect: ConnectFactory,
    config: WorkerConfig,
    activity_types: Iterable[str],
    available_handlers: Iterable[str],
    backoff: ReconnectBackoff,
    sleep: SleepFactory = asyncio.sleep,
) -> WorkerSession:
    """Connect, handshake, and register with bounded exponential backoff.

    Matches the Rust reference: the backoff loop wraps connect AND
    handshake/register so that a server that accepts TCP but rejects
    handshakes backs off exponentially rather than hammering at
    initial_backoff_seconds.
    """

    last_error: BaseException | None = None
    for attempt in range(1, backoff.max_attempts + 1):
        try:
            session = await connect()
            await session.handshake(config)
            await session.register(activity_types, available_handlers)
            return session
        except Exception as exc:
            last_error = exc
            if attempt == backoff.max_attempts:
                break
            await sleep(backoff.delay_for_attempt(attempt))
    raise ReconnectError("worker reconnect attempts exhausted") from last_error


async def reconnect_register_and_replay(
    connect: ConnectFactory,
    config: WorkerConfig,
    activity_types: Iterable[str],
    available_handlers: Iterable[str],
    tracker: UnackedResultTracker,
    sleep: SleepFactory = asyncio.sleep,
) -> WorkerSession:
    """Reconnect, re-register, then re-report backlog before serving tasks."""

    backoff = ReconnectBackoff.from_config(config)
    session = await reconnect_with_backoff(
        connect, config, activity_types, available_handlers, backoff, sleep,
    )
    await re_report_unacked(session, tracker)
    return session


async def re_report_unacked(session: WorkerSession, tracker: UnackedResultTracker) -> None:
    """Re-send every unacknowledged report in deterministic sequence order."""

    for report in tracker.snapshot():
        if isinstance(report, PendingCompletedReport):
            await session.report_result(report.workflow_id, report.activity_id, report.output)
        else:
            await session.report_failure(report.workflow_id, report.activity_id, report.failure)


def activity_sequence(activity_id: ActivityId) -> ActivitySequence:
    return int(activity_id.sequence_position)
