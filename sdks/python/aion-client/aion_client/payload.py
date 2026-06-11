"""Typed JSON and raw bytes helpers for Aion ``Payload`` values."""

from __future__ import annotations

import json
from dataclasses import dataclass, is_dataclass
from typing import Any, TypeAlias, TypeVar, cast, overload

from .errors import InvalidArgument

JSONPrimitive: TypeAlias = str | int | float | bool | None
JSONValue: TypeAlias = JSONPrimitive | list["JSONValue"] | dict[str, "JSONValue"]
JSON_CONTENT_TYPE = "application/json"

T = TypeVar("T")


@dataclass(frozen=True, slots=True)
class Payload:
    """Wire-compatible payload: content type plus opaque bytes."""

    content_type: str
    bytes: bytes


def json_payload(value: JSONValue) -> Payload:
    """Serialize a JSON-serialisable value to an Aion JSON payload.

    Raises:
        InvalidArgument: if ``value`` cannot be represented as JSON.
    """

    try:
        encoded = json.dumps(value, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    except (TypeError, ValueError) as exc:
        raise InvalidArgument(f"payload is not JSON serialisable: {exc}") from exc
    return Payload(content_type=JSON_CONTENT_TYPE, bytes=encoded)


def raw_payload(data: bytes, content_type: str) -> Payload:
    """Create a raw payload escape hatch without JSON encoding.

    Raises:
        InvalidArgument: if ``content_type`` is empty.
    """

    if not content_type:
        raise InvalidArgument("raw payload content_type must not be empty")
    return Payload(content_type=content_type, bytes=data)


def ensure_payload(
    value: JSONValue | None = None,
    *,
    raw: bytes | None = None,
    content_type: str | None = None,
) -> Payload:
    """Build a payload from either JSON ``value`` or raw bytes.

    Raises:
        InvalidArgument: if both typed and raw surfaces are supplied or if JSON
            encoding/raw validation fails.
    """

    if raw is not None:
        if value is not None:
            raise InvalidArgument("provide either a JSON value or raw bytes, not both")
        return raw_payload(raw, content_type or "application/octet-stream")
    return json_payload(value)


@overload
def decode_payload(payload: Payload, target_type: None = None) -> JSONValue | bytes: ...


@overload
def decode_payload(payload: Payload, target_type: type[T]) -> T: ...


def decode_payload(payload: Payload, target_type: type[T] | None = None) -> T | JSONValue | bytes:
    """Decode a payload into JSON, a target type, or a Pydantic/dataclass model.

    Non-JSON content returns raw bytes when no target type is supplied. Targeted
    decoding requires JSON content so that failures remain explicit.

    Raises:
        InvalidArgument: if JSON parsing or target construction fails.
    """

    if payload.content_type != JSON_CONTENT_TYPE:
        if target_type is None:
            return payload.bytes
        raise InvalidArgument(f"cannot decode {payload.content_type!r} payload as {target_type!r}")

    try:
        decoded = json.loads(payload.bytes.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise InvalidArgument(f"payload is not valid JSON: {exc}") from exc

    if target_type is None:
        return cast(JSONValue, decoded)
    return _coerce(decoded, payload.bytes, target_type)


def payload_from_wire(value: Any) -> Payload:
    """Convert generated protobuf/dict-like payload values to ``Payload``.

    Raises:
        InvalidArgument: if the value does not expose the AW ``Payload`` shape.
    """

    content_type = getattr(value, "content_type", None)
    data = getattr(value, "bytes", None)
    if isinstance(value, dict):
        content_type = value.get("content_type")
        data = value.get("bytes")
    if isinstance(content_type, str) and isinstance(data, bytes):
        return Payload(content_type=content_type, bytes=data)
    raise InvalidArgument("wire payload is missing content_type or bytes")


def assign_payload(message_payload: Any, payload: Payload) -> None:
    """Copy a ``Payload`` into a generated protobuf payload message."""

    message_payload.content_type = payload.content_type
    message_payload.bytes = payload.bytes


def _coerce(decoded: Any, raw_json: bytes, target_type: type[T]) -> T:
    model_validate_json = getattr(target_type, "model_validate_json", None)
    if callable(model_validate_json):
        try:
            return cast(T, model_validate_json(raw_json))
        except ValueError as exc:
            raise InvalidArgument(f"payload failed model validation: {exc}") from exc

    parse_raw = getattr(target_type, "parse_raw", None)
    if callable(parse_raw):
        try:
            return cast(T, parse_raw(raw_json))
        except ValueError as exc:
            raise InvalidArgument(f"payload failed model validation: {exc}") from exc

    try:
        if target_type is dict:
            if not isinstance(decoded, dict):
                raise TypeError("JSON payload is not an object")
            return cast(T, decoded)
        if target_type is list:
            if not isinstance(decoded, list):
                raise TypeError("JSON payload is not an array")
            return cast(T, decoded)
        if target_type in {str, int, float, bool}:
            if not isinstance(decoded, target_type):
                raise TypeError(f"JSON payload is not {target_type.__name__}")
            return decoded
        callable_target = cast(Any, target_type)
        if is_dataclass(target_type):
            if not isinstance(decoded, dict):
                raise TypeError("dataclass payload must be a JSON object")
            return cast(T, callable_target(**decoded))
        if isinstance(decoded, target_type):
            return decoded
        if isinstance(decoded, dict):
            return cast(T, callable_target(**decoded))
        return cast(T, callable_target(decoded))
    except (TypeError, ValueError) as exc:
        raise InvalidArgument(f"payload decode failed for {target_type!r}: {exc}") from exc
