"""WorkflowHandle bound operations for the Aion Python client."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, TypeVar

from .errors import InvalidArgument
from .payload import JSONValue
from .stream import EventStream

if TYPE_CHECKING:
    from .client import Client, WorkflowDescription

T = TypeVar("T")


@dataclass(frozen=True, slots=True)
class WorkflowHandle:
    """Handle returned by ``Client.start`` and created from bare workflow IDs."""

    client: Client
    workflow_id: str
    run_id: str | None
    namespace: str

    def latest_run(self) -> WorkflowHandle:
        """Return a handle that targets the latest run by omitting ``run_id``."""

        return WorkflowHandle(client=self.client, workflow_id=self.workflow_id, run_id=None, namespace=self.namespace)

    def specific_run(self, run_id: str) -> WorkflowHandle:
        """Return a handle targeting exactly ``run_id``.

        Raises:
            InvalidArgument: run_id is empty.
        """

        if not run_id:
            raise InvalidArgument("run_id must not be empty")
        return WorkflowHandle(client=self.client, workflow_id=self.workflow_id, run_id=run_id, namespace=self.namespace)

    async def signal(
        self,
        signal_name: str,
        payload: JSONValue | None = None,
        *,
        raw: bytes | None = None,
        content_type: str | None = None,
    ) -> None:
        """Send a signal to this workflow handle's target run.

        Raises:
            NotFound, Unauthenticated, NamespaceDenied, Unavailable,
            InvalidArgument, ServerError, Cancelled.
        """

        await self.client.signal(
            self.workflow_id,
            signal_name,
            payload,
            raw=raw,
            content_type=content_type,
            run_id=self.run_id,
            namespace=self.namespace,
        )

    async def query(
        self,
        query_name: str,
        payload: JSONValue | None = None,
        *,
        raw: bytes | None = None,
        content_type: str | None = None,
        target_type: type[T] | None = None,
        timeout: float | None = None,
    ) -> T | JSONValue | bytes:
        """Run a synchronous query against this workflow handle's target run.

        Raises:
            NotFound, QueryFailed, QueryTimeout, Unauthenticated,
            NamespaceDenied, Unavailable, InvalidArgument, ServerError,
            Cancelled.
        """

        return await self.client.query(
            self.workflow_id,
            query_name,
            payload,
            raw=raw,
            content_type=content_type,
            run_id=self.run_id,
            namespace=self.namespace,
            target_type=target_type,
            timeout=timeout,
        )

    async def cancel(self, *, reason: str = "") -> None:
        """Request cooperative cancellation for this workflow handle's target run.

        Raises:
            NotFound, Unauthenticated, NamespaceDenied, Unavailable,
            InvalidArgument, ServerError, Cancelled.
        """

        await self.client.cancel(self.workflow_id, run_id=self.run_id, reason=reason, namespace=self.namespace)

    async def describe(self, *, include_history: bool = False) -> WorkflowDescription:
        """Describe this workflow handle's target run.

        Raises:
            NotFound, Unauthenticated, NamespaceDenied, Unavailable,
            InvalidArgument, ServerError, Cancelled.
        """

        return await self.client.describe(
            self.workflow_id,
            run_id=self.run_id,
            include_history=include_history,
            namespace=self.namespace,
        )

    def subscribe(
        self,
        *,
        decoder: type[T] | None = None,
        raw: bool = False,
        from_seq: int | None = None,
    ) -> EventStream[T]:
        """Return an async iterator of workflow events.

        The initial attach is a live tail (events recorded from now on)
        unless ``from_seq`` supplies an explicit starting cursor:
        ``from_seq=1`` replays the full recorded history before splicing
        into the live stream. Transient disconnects reconnect transparently
        and resume from the last delivered per-workflow sequence number via
        the wire cursor (``resume_from_seq`` = last delivered + 1), so
        delivery stays gap-free and duplicate-free across reconnects.

        Raises:
            InvalidArgument: no ``stream_endpoint`` was configured on the
                client, or ``from_seq`` is below 1.

        Raises from iteration:
            NotFound, Unauthenticated, NamespaceDenied, Unavailable,
            InvalidArgument, ServerError, Cancelled.
        """

        stream_endpoint = self.client.stream_endpoint
        if stream_endpoint is None:
            raise InvalidArgument(
                "no stream endpoint is configured; event subscriptions require "
                "Client(stream_endpoint=...) pointing at the server's "
                "/events/stream WebSocket URL (the HTTP/WebSocket listener is "
                "a separate address from the gRPC endpoint)"
            )
        return EventStream(
            endpoint=stream_endpoint,
            namespace=self.namespace,
            workflow_id=self.workflow_id,
            run_id=self.run_id,
            auth=self.client.auth,
            subject=self.client.subject,
            namespaces=self.client.namespaces,
            decoder=decoder,
            raw=raw,
            from_seq=from_seq,
        )
