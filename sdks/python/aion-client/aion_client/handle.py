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

    def subscribe(self, *, decoder: type[T] | None = None, raw: bool = False) -> EventStream[T]:
        """Return an async iterator of events with transparent resumption.

        Raises from iteration:
            NotFound, Unauthenticated, NamespaceDenied, Unavailable,
            InvalidArgument, ServerError, Cancelled.
        """

        return EventStream(
            endpoint=self.client.events_endpoint,
            namespace=self.namespace,
            workflow_id=self.workflow_id,
            run_id=self.run_id,
            auth=self.client.auth,
            decoder=decoder,
            raw=raw,
        )
