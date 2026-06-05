from __future__ import annotations

from collections.abc import AsyncIterator

import pytest

from aion_client import Unavailable
from aion_client.stream import EventStream, TerminalStreamFailure, TransientStreamDisconnect


async def _iter_events(events: list[dict[str, int]], *, drop_after_first: bool = False) -> AsyncIterator[dict[str, int]]:
    for index, event in enumerate(events):
        if drop_after_first and index == 1:
            raise TransientStreamDisconnect("dropped")
        yield event


@pytest.mark.asyncio
async def test_stream_resumes_and_skips_duplicates() -> None:
    resume_requests: list[int | None] = []

    async def factory(resume_from: int | None) -> AsyncIterator[dict[str, int]]:
        resume_requests.append(resume_from)
        if resume_from is None:
            return _iter_events([{"seq": 1}, {"seq": 2}], drop_after_first=True)
        return _iter_events([{"seq": 1}, {"seq": 2}, {"seq": 3}])

    stream: EventStream[dict[str, int]] = EventStream(
        endpoint="ws://example/events",
        namespace="default",
        workflow_id="wf",
        run_id="run",
        auth=None,
        transport_factory=factory,
    )

    seen = [await stream.__anext__(), await stream.__anext__(), await stream.__anext__()]
    assert seen == [{"seq": 1}, {"seq": 2}, {"seq": 3}]
    assert resume_requests == [None, 2]


@pytest.mark.asyncio
async def test_stream_terminal_failure_raises() -> None:
    async def broken(_: int | None) -> AsyncIterator[dict[str, int]]:
        raise TerminalStreamFailure("terminal")
        yield {"seq": 1}

    stream: EventStream[dict[str, int]] = EventStream(
        endpoint="ws://example/events",
        namespace="default",
        workflow_id="wf",
        run_id=None,
        auth=None,
        transport_factory=broken,
    )

    with pytest.raises(Unavailable):
        await stream.__anext__()
