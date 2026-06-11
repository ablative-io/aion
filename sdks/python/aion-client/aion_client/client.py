"""Async Aion workflow client for the caller-side Python SDK."""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass
from types import TracebackType
from typing import Any, TypeVar, cast

from .errors import AionClientError, AlreadyExists, InvalidArgument, QueryFailed, map_query_error, raise_mapped
from .handle import WorkflowHandle
from .payload import JSONValue, Payload, assign_payload, decode_payload, ensure_payload, payload_from_wire
from .transport import GrpcWorkflowTransport, MappingMetadata, WorkflowTransport, metadata

T = TypeVar("T")


@dataclass(frozen=True, slots=True)
class TLSConfig:
    """TLS options used when creating the reusable transport connection."""

    enabled: bool = True
    ca_file: str | None = None
    client_cert_file: str | None = None
    client_key_file: str | None = None
    server_name: str | None = None


@dataclass(frozen=True, slots=True)
class WorkflowDescription:
    """Decoded describe response containing summary and optional history."""

    summary: Any
    history: list[Any]


class Client:
    """Reusable async client for Aion workflow operations.

    Public methods raise branchable subclasses of ``AionClientError`` according
    to the shared taxonomy: NotFound, AlreadyExists, QueryFailed, QueryTimeout,
    Cancelled, Unavailable, Unauthenticated, NamespaceDenied, InvalidArgument,
    and ServerError.
    """

    def __init__(
        self,
        endpoint: str,
        *,
        auth: str | None = None,
        tls: TLSConfig | bool | None = None,
        namespace: str = "default",
        transport: WorkflowTransport | None = None,
        stream_endpoint: str | None = None,
        subject: str | None = None,
        namespaces: Sequence[str] | None = None,
    ) -> None:
        """Create a reusable client connection.

        ``stream_endpoint`` is the full URL of the server's
        ``/events/stream`` WebSocket route (for example
        ``ws://127.0.0.1:8080/events/stream``). There is no default and
        nothing is derived: the gRPC endpoint and the HTTP/WebSocket
        listener are separate addresses, so ``subscribe`` without this
        option raises :class:`InvalidArgument` with a precise message.

        Raises:
            InvalidArgument, Unavailable, Unauthenticated, ServerError,
            Cancelled.
        """

        if not endpoint:
            raise InvalidArgument("endpoint must not be empty")
        if not namespace:
            raise InvalidArgument("namespace must not be empty")
        self.endpoint = endpoint.rstrip("/")
        self.namespace = namespace
        self.auth = auth
        self.subject = subject
        self.namespaces = tuple(namespaces) if namespaces is not None else None
        self.tls = _normalize_tls(tls)
        self._transport = transport or GrpcWorkflowTransport(self.endpoint, tls=self.tls)
        self.stream_endpoint = stream_endpoint
        self._metadata = metadata(auth, subject=subject, namespaces=self.namespaces)
        # SDK-boundary start idempotency (the contract's hard case): the same
        # key retried with an identical request returns the original handle;
        # conflicting reuse raises AlreadyExists. Keyed by idempotency key,
        # fingerprinted over namespace, workflow type, and encoded input.
        self._idempotent_starts: dict[str, tuple[tuple[str, str, str, bytes], WorkflowHandle]] = {}

    @classmethod
    async def connect(
        cls,
        endpoint: str,
        *,
        auth: str | None = None,
        tls: TLSConfig | bool | None = None,
        namespace: str = "default",
        transport: WorkflowTransport | None = None,
        stream_endpoint: str | None = None,
        subject: str | None = None,
        namespaces: Sequence[str] | None = None,
    ) -> Client:
        """Create a client and validate the reusable connection if supported.

        Raises:
            InvalidArgument, Unavailable, Unauthenticated, ServerError,
            Cancelled.
        """

        client = cls(
            endpoint,
            auth=auth,
            tls=tls,
            namespace=namespace,
            transport=transport,
            stream_endpoint=stream_endpoint,
            subject=subject,
            namespaces=namespaces,
        )
        connect = getattr(client._transport, "connect", None)
        if callable(connect):
            try:
                await connect()
            except BaseException as exc:
                if isinstance(exc, AionClientError):
                    raise
                raise_mapped(exc)
        return client

    async def __aenter__(self) -> Client:
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        await self.close()

    async def close(self) -> None:
        """Close the reusable transport.

        Raises:
            Unavailable, ServerError, Cancelled.
        """

        try:
            await self._transport.close()
        except BaseException as exc:
            if isinstance(exc, AionClientError):
                raise
            raise_mapped(exc)

    async def start(
        self,
        workflow_type: str,
        input: JSONValue | None = None,
        *,
        raw: bytes | None = None,
        content_type: str | None = None,
        namespace: str | None = None,
        idempotency_key: str | None = None,
    ) -> WorkflowHandle:
        """Start a workflow and return a bound ``WorkflowHandle``.

        Raises:
            AlreadyExists, Unauthenticated, NamespaceDenied, Unavailable,
            InvalidArgument, ServerError, Cancelled.
        """

        if not workflow_type:
            raise InvalidArgument("workflow_type must not be empty")
        payload = ensure_payload(input, raw=raw, content_type=content_type)
        fingerprint: tuple[str, str, str, bytes] | None = None
        if idempotency_key is not None:
            fingerprint = (namespace or self.namespace, workflow_type, payload.content_type, bytes(payload.bytes))
            cached = self._idempotent_starts.get(idempotency_key)
            if cached is not None:
                cached_fingerprint, cached_handle = cached
                if cached_fingerprint == fingerprint:
                    return cached_handle
                raise _idempotency_conflict()
        request = self._message("StartWorkflowRequest")
        request.namespace = namespace or self.namespace
        request.workflow_type = workflow_type
        assign_payload(request.input, payload)
        try:
            response = await self._transport.start_workflow(
                request,
                self._call_metadata(idempotency_key=idempotency_key),
            )
            workflow_id = _id_value(response.workflow_id)
            run_id = _id_value(response.run_id)
        except BaseException as exc:
            if isinstance(exc, AionClientError):
                raise
            raise_mapped(exc, operation="start")
        handle = WorkflowHandle(
            client=self,
            workflow_id=workflow_id,
            run_id=run_id,
            namespace=namespace or self.namespace,
        )
        if idempotency_key is not None and fingerprint is not None:
            recorded = self._idempotent_starts.get(idempotency_key)
            if recorded is not None and recorded[0] != fingerprint:
                raise _idempotency_conflict()
            self._idempotent_starts.setdefault(idempotency_key, (fingerprint, handle))
        return handle

    async def signal(
        self,
        workflow_id: str,
        signal_name: str,
        payload: JSONValue | None = None,
        *,
        raw: bytes | None = None,
        content_type: str | None = None,
        run_id: str | None = None,
        namespace: str | None = None,
    ) -> None:
        """Send a signal to a workflow run or latest run.

        Raises:
            NotFound, Unauthenticated, NamespaceDenied, Unavailable,
            InvalidArgument, ServerError, Cancelled.
        """

        if not workflow_id or not signal_name:
            raise InvalidArgument("workflow_id and signal_name must not be empty")
        request = self._targeted_message("SignalRequest", namespace, workflow_id, run_id)
        request.signal_name = signal_name
        assign_payload(request.payload, ensure_payload(payload, raw=raw, content_type=content_type))
        await self._call("signal", self._transport.signal(request, self._metadata))

    async def query(
        self,
        workflow_id: str,
        query_name: str,
        payload: JSONValue | None = None,
        *,
        raw: bytes | None = None,
        content_type: str | None = None,
        run_id: str | None = None,
        namespace: str | None = None,
        target_type: type[T] | None = None,
        timeout: float | None = None,
    ) -> T | JSONValue | bytes:
        """Run a synchronous workflow query and decode its result.

        Query arguments are validated now and will be forwarded once AW exposes
        a query payload field in ``QueryRequest``.

        Raises:
            NotFound, QueryFailed, QueryTimeout, Unauthenticated,
            NamespaceDenied, Unavailable, InvalidArgument, ServerError,
            Cancelled.
        """

        if not workflow_id or not query_name:
            raise InvalidArgument("workflow_id and query_name must not be empty")
        if payload is not None or raw is not None:
            ensure_payload(payload, raw=raw, content_type=content_type)
        request = self._targeted_message("QueryRequest", namespace, workflow_id, run_id)
        request.query_name = query_name
        try:
            response = await self._transport.query(request, self._metadata, timeout=timeout)
            outcome = _which_oneof(response, "outcome")
            if outcome == "error":
                raise map_query_error(response.error)
            if outcome != "result":
                raise QueryFailed("query response did not contain a result")
            return decode_payload(payload_from_wire(response.result), target_type)
        except BaseException as exc:
            if isinstance(exc, AionClientError):
                raise
            raise_mapped(exc, operation="query")

    async def cancel(
        self,
        workflow_id: str,
        *,
        run_id: str | None = None,
        reason: str = "",
        namespace: str | None = None,
    ) -> None:
        """Request cooperative cancellation of a workflow run or latest run.

        Raises:
            NotFound, Unauthenticated, NamespaceDenied, Unavailable,
            InvalidArgument, ServerError, Cancelled.
        """

        request = self._targeted_message("CancelRequest", namespace, workflow_id, run_id)
        request.reason = reason
        await self._call("cancel", self._transport.cancel(request, self._metadata))

    async def list(self, *, namespace: str | None = None, workflow_filter: Any | None = None) -> list[Any]:
        """List workflow summaries in the namespace.

        Raises:
            Unauthenticated, NamespaceDenied, Unavailable, InvalidArgument,
            ServerError, Cancelled.
        """

        request = self._message("ListWorkflowsRequest")
        request.namespace = namespace or self.namespace
        if workflow_filter is not None:
            _assign_wire_envelope(request.filter, namespace or self.namespace, workflow_filter)
        response = await self._call("list", self._transport.list_workflows(request, self._metadata))
        return list(getattr(response, "summaries", []))

    async def describe(
        self,
        workflow_id: str,
        *,
        run_id: str | None = None,
        include_history: bool = False,
        namespace: str | None = None,
    ) -> WorkflowDescription:
        """Describe workflow state and optional event history.

        Raises:
            NotFound, Unauthenticated, NamespaceDenied, Unavailable,
            InvalidArgument, ServerError, Cancelled.
        """

        request = self._targeted_message("DescribeWorkflowRequest", namespace, workflow_id, run_id)
        request.include_history = include_history
        response = await self._call("describe", self._transport.describe_workflow(request, self._metadata))
        return WorkflowDescription(summary=response.summary, history=list(getattr(response, "history", [])))

    def handle(self, workflow_id: str, *, run_id: str | None = None, namespace: str | None = None) -> WorkflowHandle:
        """Construct a bare-ID workflow handle targeting latest or a concrete run.

        Raises:
            InvalidArgument: workflow_id is empty.
        """

        if not workflow_id:
            raise InvalidArgument("workflow_id must not be empty")
        return WorkflowHandle(
            client=self,
            workflow_id=workflow_id,
            run_id=run_id,
            namespace=namespace or self.namespace,
        )

    def _targeted_message(self, name: str, namespace: str | None, workflow_id: str, run_id: str | None) -> Any:
        if not workflow_id:
            raise InvalidArgument("workflow_id must not be empty")
        request = self._message(name)
        request.namespace = namespace or self.namespace
        _set_id(request.workflow_id, workflow_id)
        if run_id is not None:
            _set_id(request.run_id, run_id)
        return request

    def _message(self, name: str) -> Any:
        message_factory = getattr(self._transport, "message", None)
        if callable(message_factory):
            return message_factory(name)
        return GrpcWorkflowTransport.message(name)

    def _call_metadata(self, *, idempotency_key: str | None = None) -> MappingMetadata:
        if idempotency_key is None:
            return self._metadata
        return (*self._metadata, ("x-aion-idempotency-key", idempotency_key))

    async def _call(self, operation: str, awaitable: Any) -> Any:
        try:
            return await awaitable
        except BaseException as exc:
            if isinstance(exc, AionClientError):
                raise
            raise_mapped(exc, operation=operation)


def _idempotency_conflict() -> AlreadyExists:
    """The SDK-boundary idempotency conflict: the same key was reused with a
    different start request."""

    return AlreadyExists(
        "idempotency key was already used by a different start request (namespace, workflow type, or input differ)"
    )


def _normalize_tls(tls: TLSConfig | bool | None) -> TLSConfig:
    if tls is None:
        return TLSConfig()
    if isinstance(tls, bool):
        return TLSConfig(enabled=tls)
    return tls


def _id_value(value: Any) -> str:
    uuid = getattr(value, "uuid", None)
    if not isinstance(uuid, str) or not uuid:
        raise InvalidArgument("server returned an empty workflow/run id")
    return uuid


def _set_id(target: Any, value: str) -> None:
    if not value:
        raise InvalidArgument("id value must not be empty")
    target.uuid = value


def _which_oneof(message: Any, name: str) -> str | None:
    which = getattr(message, "WhichOneof", None)
    if callable(which):
        result = which(name)
        return str(result) if result is not None else None
    if getattr(message, "error", None) is not None:
        return "error"
    if getattr(message, "result", None) is not None:
        return "result"
    return None


def _assign_wire_envelope(envelope: Any, namespace: str, value: Any) -> None:
    envelope.namespace = namespace
    if isinstance(value, Payload):
        assign_payload(envelope.payload, value)
    else:
        assign_payload(envelope.payload, ensure_payload(cast(JSONValue, value)))
