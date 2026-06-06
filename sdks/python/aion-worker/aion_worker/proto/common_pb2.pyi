from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class WorkflowStatus(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    WORKFLOW_STATUS_UNSPECIFIED: _ClassVar[WorkflowStatus]
    WORKFLOW_STATUS_RUNNING: _ClassVar[WorkflowStatus]
    WORKFLOW_STATUS_COMPLETED: _ClassVar[WorkflowStatus]
    WORKFLOW_STATUS_FAILED: _ClassVar[WorkflowStatus]
    WORKFLOW_STATUS_CANCELLED: _ClassVar[WorkflowStatus]
    WORKFLOW_STATUS_TIMED_OUT: _ClassVar[WorkflowStatus]

class WireErrorCode(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    WIRE_ERROR_CODE_UNSPECIFIED: _ClassVar[WireErrorCode]
    WIRE_ERROR_CODE_NOT_FOUND: _ClassVar[WireErrorCode]
    WIRE_ERROR_CODE_NAMESPACE_DENIED: _ClassVar[WireErrorCode]
    WIRE_ERROR_CODE_SEQUENCE_CONFLICT: _ClassVar[WireErrorCode]
    WIRE_ERROR_CODE_UNKNOWN_QUERY: _ClassVar[WireErrorCode]
    WIRE_ERROR_CODE_QUERY_TIMEOUT: _ClassVar[WireErrorCode]
    WIRE_ERROR_CODE_NOT_RUNNING: _ClassVar[WireErrorCode]
    WIRE_ERROR_CODE_LAGGED: _ClassVar[WireErrorCode]
    WIRE_ERROR_CODE_BACKEND: _ClassVar[WireErrorCode]
WORKFLOW_STATUS_UNSPECIFIED: WorkflowStatus
WORKFLOW_STATUS_RUNNING: WorkflowStatus
WORKFLOW_STATUS_COMPLETED: WorkflowStatus
WORKFLOW_STATUS_FAILED: WorkflowStatus
WORKFLOW_STATUS_CANCELLED: WorkflowStatus
WORKFLOW_STATUS_TIMED_OUT: WorkflowStatus
WIRE_ERROR_CODE_UNSPECIFIED: WireErrorCode
WIRE_ERROR_CODE_NOT_FOUND: WireErrorCode
WIRE_ERROR_CODE_NAMESPACE_DENIED: WireErrorCode
WIRE_ERROR_CODE_SEQUENCE_CONFLICT: WireErrorCode
WIRE_ERROR_CODE_UNKNOWN_QUERY: WireErrorCode
WIRE_ERROR_CODE_QUERY_TIMEOUT: WireErrorCode
WIRE_ERROR_CODE_NOT_RUNNING: WireErrorCode
WIRE_ERROR_CODE_LAGGED: WireErrorCode
WIRE_ERROR_CODE_BACKEND: WireErrorCode

class WorkflowId(_message.Message):
    __slots__ = ("uuid",)
    UUID_FIELD_NUMBER: _ClassVar[int]
    uuid: str
    def __init__(self, uuid: _Optional[str] = ...) -> None: ...

class RunId(_message.Message):
    __slots__ = ("uuid",)
    UUID_FIELD_NUMBER: _ClassVar[int]
    uuid: str
    def __init__(self, uuid: _Optional[str] = ...) -> None: ...

class ActivityId(_message.Message):
    __slots__ = ("sequence_position",)
    SEQUENCE_POSITION_FIELD_NUMBER: _ClassVar[int]
    sequence_position: int
    def __init__(self, sequence_position: _Optional[int] = ...) -> None: ...

class TimerId(_message.Message):
    __slots__ = ("name", "sequence_position")
    NAME_FIELD_NUMBER: _ClassVar[int]
    SEQUENCE_POSITION_FIELD_NUMBER: _ClassVar[int]
    name: str
    sequence_position: int
    def __init__(self, name: _Optional[str] = ..., sequence_position: _Optional[int] = ...) -> None: ...

class Payload(_message.Message):
    __slots__ = ("content_type", "bytes")
    CONTENT_TYPE_FIELD_NUMBER: _ClassVar[int]
    BYTES_FIELD_NUMBER: _ClassVar[int]
    content_type: str
    bytes: bytes
    def __init__(self, content_type: _Optional[str] = ..., bytes: _Optional[bytes] = ...) -> None: ...

class WireEnvelope(_message.Message):
    __slots__ = ("namespace", "request_id", "payload")
    NAMESPACE_FIELD_NUMBER: _ClassVar[int]
    REQUEST_ID_FIELD_NUMBER: _ClassVar[int]
    PAYLOAD_FIELD_NUMBER: _ClassVar[int]
    namespace: str
    request_id: str
    payload: Payload
    def __init__(self, namespace: _Optional[str] = ..., request_id: _Optional[str] = ..., payload: _Optional[_Union[Payload, _Mapping]] = ...) -> None: ...

class WireError(_message.Message):
    __slots__ = ("code", "message")
    CODE_FIELD_NUMBER: _ClassVar[int]
    MESSAGE_FIELD_NUMBER: _ClassVar[int]
    code: WireErrorCode
    message: str
    def __init__(self, code: _Optional[_Union[WireErrorCode, str]] = ..., message: _Optional[str] = ...) -> None: ...
