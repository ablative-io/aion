"""ActivityContext for explicit heartbeat and cooperative cancellation."""

from __future__ import annotations

import asyncio
from typing import Any

from .proto import common_pb2
from .session import ActivityId, Payload, WorkerSession, WorkflowId

JSON_CONTENT_TYPE = "application/json"
WIRE_DEFAULT_ATTEMPT = 1


class ActivityContext:
    """Context passed to each activity handler."""

    def __init__(
        self,
        *,
        workflow_id: WorkflowId,
        activity_id: ActivityId,
        attempt: int = WIRE_DEFAULT_ATTEMPT,
        session: WorkerSession,
        content_type: str = JSON_CONTENT_TYPE,
    ) -> None:
        self.workflow_id = workflow_id
        self.activity_id = activity_id
        self.attempt = attempt
        self._session = session
        self._content_type = content_type
        self._cancelled = asyncio.Event()

    async def heartbeat(self, detail: Payload | Any | None = None) -> None:
        """Send explicit heartbeat progress for this activity."""

        progress = encode_detail(detail, self._content_type)
        await self._session.send_heartbeat(self.workflow_id, self.activity_id, progress)

    def is_cancelled(self) -> bool:
        """Return whether the engine has requested cooperative cancellation."""

        return self._cancelled.is_set()

    async def cancelled(self) -> None:
        """Wait until the engine requests cooperative cancellation."""

        await self._cancelled.wait()

    def _cancel(self) -> None:
        self._cancelled.set()


class ActivityCancellationHandle:
    """Internal handle used by the receive loop to deliver cancellation."""

    def __init__(self, context: ActivityContext) -> None:
        self._context = context

    def cancel(self) -> None:
        """Mark the associated context cancelled without cancelling its task."""

        self._context._cancel()


def encode_detail(detail: Payload | Any | None, content_type: str = JSON_CONTENT_TYPE) -> Payload | None:
    """Encode heartbeat/error detail without changing existing Payload content types."""

    if detail is None:
        return None
    if isinstance(detail, common_pb2.Payload):
        return detail
    from .activity import encode_json_payload

    return encode_json_payload(detail, content_type)
