"""Reusable transport helpers for the Aion Python client."""

from __future__ import annotations

import importlib
from collections.abc import Sequence
from types import ModuleType
from typing import Any, Protocol
from urllib.parse import urlparse

from .errors import InvalidArgument

MappingMetadata = tuple[tuple[str, str], ...]


class WorkflowTransport(Protocol):
    """Protocol implemented by real/stub reusable workflow transports."""

    async def start_workflow(self, request: Any, metadata: MappingMetadata) -> Any: ...
    async def signal(self, request: Any, metadata: MappingMetadata) -> Any: ...
    async def query(self, request: Any, metadata: MappingMetadata, timeout: float | None = None) -> Any: ...
    async def cancel(self, request: Any, metadata: MappingMetadata) -> Any: ...
    async def list_workflows(self, request: Any, metadata: MappingMetadata) -> Any: ...
    async def describe_workflow(self, request: Any, metadata: MappingMetadata) -> Any: ...
    async def close(self) -> None: ...


class GrpcWorkflowTransport:
    """gRPC transport backed by generated ``aion`` protobuf stubs."""

    def __init__(self, endpoint: str, *, tls: Any) -> None:
        try:
            grpc = importlib.import_module("grpc")
            workflow_pb2_grpc = importlib.import_module("aion_client.proto.workflow_pb2_grpc")
        except ImportError as exc:
            raise InvalidArgument(
                "grpcio and the packaged aion_client.proto stubs are required for the default transport"
            ) from exc
        self._grpc: ModuleType = grpc
        self._workflow_pb2_grpc: ModuleType = workflow_pb2_grpc
        self._channel = self._build_channel(endpoint, tls)
        self._stub = workflow_pb2_grpc.WorkflowServiceStub(self._channel)

    @staticmethod
    def message(name: str) -> Any:
        """Create a generated AW workflow protobuf message by name."""

        try:
            workflow_pb2 = importlib.import_module("aion_client.proto.workflow_pb2")
        except ImportError as exc:
            raise InvalidArgument("packaged aion_client.proto.workflow_pb2 module is required") from exc
        try:
            cls = getattr(workflow_pb2, name)
        except AttributeError as exc:
            raise InvalidArgument(f"generated proto message {name} is unavailable") from exc
        return cls()

    async def connect(self) -> None:
        """Await channel readiness when the gRPC runtime exposes it."""

        channel_ready = getattr(self._channel, "channel_ready", None)
        if callable(channel_ready):
            await channel_ready()

    async def start_workflow(self, request: Any, metadata: MappingMetadata) -> Any:
        return await self._stub.StartWorkflow(request, metadata=metadata)

    async def signal(self, request: Any, metadata: MappingMetadata) -> Any:
        return await self._stub.Signal(request, metadata=metadata)

    async def query(self, request: Any, metadata: MappingMetadata, timeout: float | None = None) -> Any:
        return await self._stub.Query(request, metadata=metadata, timeout=timeout)

    async def cancel(self, request: Any, metadata: MappingMetadata) -> Any:
        return await self._stub.Cancel(request, metadata=metadata)

    async def list_workflows(self, request: Any, metadata: MappingMetadata) -> Any:
        return await self._stub.ListWorkflows(request, metadata=metadata)

    async def describe_workflow(self, request: Any, metadata: MappingMetadata) -> Any:
        return await self._stub.DescribeWorkflow(request, metadata=metadata)

    async def close(self) -> None:
        await self._channel.close()

    def _build_channel(self, endpoint: str, tls: Any) -> Any:
        target = grpc_target(endpoint)
        if tls.enabled:
            credentials = self._grpc.ssl_channel_credentials(
                root_certificates=read_optional(tls.ca_file),
                private_key=read_optional(tls.client_key_file),
                certificate_chain=read_optional(tls.client_cert_file),
            )
            options: list[tuple[str, str]] = []
            if tls.server_name is not None:
                options.append(("grpc.ssl_target_name_override", tls.server_name))
            return self._grpc.aio.secure_channel(target, credentials, options=options)
        return self._grpc.aio.insecure_channel(target)


def metadata(
    auth: str | None,
    *,
    subject: str | None = None,
    namespaces: Sequence[str] | None = None,
) -> MappingMetadata:
    """Build AW caller-identity metadata: the bearer credential plus the
    development-mode identity headers (``x-aion-subject``,
    ``x-aion-namespaces``) the server's caller extraction reads."""

    entries: list[tuple[str, str]] = []
    if auth is not None:
        token = auth.removeprefix("Bearer ")
        entries.append(("authorization", f"Bearer {token}"))
    if subject is not None:
        entries.append(("x-aion-subject", subject))
    if namespaces is not None:
        entries.append(("x-aion-namespaces", ",".join(namespaces)))
    return tuple(entries)


def grpc_target(endpoint: str) -> str:
    """Translate URL-style endpoints to the host:port gRPC target."""

    parsed = urlparse(endpoint)
    if parsed.scheme in {"http", "https"} and parsed.netloc:
        return parsed.netloc
    return endpoint.removeprefix("grpc://").removeprefix("grpcs://")


def read_optional(path: str | None) -> bytes | None:
    """Read optional TLS material from disk."""

    if path is None:
        return None
    with open(path, "rb") as file:
        return file.read()
