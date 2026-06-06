from collections.abc import AsyncIterator
from typing import Any, Protocol

from .worker_pb2 import ServerToWorker, WorkerToServer

class _StreamStreamCallable(Protocol):
    def __call__(self, request_iterator: AsyncIterator[WorkerToServer]) -> AsyncIterator[ServerToWorker]: ...

class WorkerProtocolStub:
    StreamWorker: _StreamStreamCallable
    def __init__(self, channel: Any) -> None: ...
