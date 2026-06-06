from . import common_pb2 as _common_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class ActivityErrorKind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    ACTIVITY_ERROR_KIND_UNSPECIFIED: _ClassVar[ActivityErrorKind]
    ACTIVITY_ERROR_KIND_RETRYABLE: _ClassVar[ActivityErrorKind]
    ACTIVITY_ERROR_KIND_TERMINAL: _ClassVar[ActivityErrorKind]
ACTIVITY_ERROR_KIND_UNSPECIFIED: ActivityErrorKind
ACTIVITY_ERROR_KIND_RETRYABLE: ActivityErrorKind
ACTIVITY_ERROR_KIND_TERMINAL: ActivityErrorKind

class WorkerToServer(_message.Message):
    __slots__ = ("register", "result", "heartbeat")
    REGISTER_FIELD_NUMBER: _ClassVar[int]
    RESULT_FIELD_NUMBER: _ClassVar[int]
    HEARTBEAT_FIELD_NUMBER: _ClassVar[int]
    register: RegisterWorker
    result: ActivityResult
    heartbeat: Heartbeat
    def __init__(self, register: _Optional[_Union[RegisterWorker, _Mapping]] = ..., result: _Optional[_Union[ActivityResult, _Mapping]] = ..., heartbeat: _Optional[_Union[Heartbeat, _Mapping]] = ...) -> None: ...

class ServerToWorker(_message.Message):
    __slots__ = ("task",)
    TASK_FIELD_NUMBER: _ClassVar[int]
    task: ActivityTask
    def __init__(self, task: _Optional[_Union[ActivityTask, _Mapping]] = ...) -> None: ...

class RegisterWorker(_message.Message):
    __slots__ = ("namespace", "activity_types")
    NAMESPACE_FIELD_NUMBER: _ClassVar[int]
    ACTIVITY_TYPES_FIELD_NUMBER: _ClassVar[int]
    namespace: str
    activity_types: _containers.RepeatedScalarFieldContainer[str]
    def __init__(self, namespace: _Optional[str] = ..., activity_types: _Optional[_Iterable[str]] = ...) -> None: ...

class ActivityTask(_message.Message):
    __slots__ = ("workflow_id", "activity_id", "activity_type", "input")
    WORKFLOW_ID_FIELD_NUMBER: _ClassVar[int]
    ACTIVITY_ID_FIELD_NUMBER: _ClassVar[int]
    ACTIVITY_TYPE_FIELD_NUMBER: _ClassVar[int]
    INPUT_FIELD_NUMBER: _ClassVar[int]
    workflow_id: _common_pb2.WorkflowId
    activity_id: _common_pb2.ActivityId
    activity_type: str
    input: _common_pb2.Payload
    def __init__(self, workflow_id: _Optional[_Union[_common_pb2.WorkflowId, _Mapping]] = ..., activity_id: _Optional[_Union[_common_pb2.ActivityId, _Mapping]] = ..., activity_type: _Optional[str] = ..., input: _Optional[_Union[_common_pb2.Payload, _Mapping]] = ...) -> None: ...

class ActivityResult(_message.Message):
    __slots__ = ("workflow_id", "activity_id", "result", "error")
    WORKFLOW_ID_FIELD_NUMBER: _ClassVar[int]
    ACTIVITY_ID_FIELD_NUMBER: _ClassVar[int]
    RESULT_FIELD_NUMBER: _ClassVar[int]
    ERROR_FIELD_NUMBER: _ClassVar[int]
    workflow_id: _common_pb2.WorkflowId
    activity_id: _common_pb2.ActivityId
    result: _common_pb2.Payload
    error: ActivityError
    def __init__(self, workflow_id: _Optional[_Union[_common_pb2.WorkflowId, _Mapping]] = ..., activity_id: _Optional[_Union[_common_pb2.ActivityId, _Mapping]] = ..., result: _Optional[_Union[_common_pb2.Payload, _Mapping]] = ..., error: _Optional[_Union[ActivityError, _Mapping]] = ...) -> None: ...

class ActivityError(_message.Message):
    __slots__ = ("kind", "message", "details")
    KIND_FIELD_NUMBER: _ClassVar[int]
    MESSAGE_FIELD_NUMBER: _ClassVar[int]
    DETAILS_FIELD_NUMBER: _ClassVar[int]
    kind: ActivityErrorKind
    message: str
    details: _common_pb2.Payload
    def __init__(self, kind: _Optional[_Union[ActivityErrorKind, str]] = ..., message: _Optional[str] = ..., details: _Optional[_Union[_common_pb2.Payload, _Mapping]] = ...) -> None: ...

class Heartbeat(_message.Message):
    __slots__ = ("workflow_id", "activity_id", "progress")
    WORKFLOW_ID_FIELD_NUMBER: _ClassVar[int]
    ACTIVITY_ID_FIELD_NUMBER: _ClassVar[int]
    PROGRESS_FIELD_NUMBER: _ClassVar[int]
    workflow_id: _common_pb2.WorkflowId
    activity_id: _common_pb2.ActivityId
    progress: _common_pb2.Payload
    def __init__(self, workflow_id: _Optional[_Union[_common_pb2.WorkflowId, _Mapping]] = ..., activity_id: _Optional[_Union[_common_pb2.ActivityId, _Mapping]] = ..., progress: _Optional[_Union[_common_pb2.Payload, _Mapping]] = ...) -> None: ...
