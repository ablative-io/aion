from __future__ import annotations

from dataclasses import dataclass
from typing import cast

import pytest

from aion_client import InvalidArgument, JSONValue, decode_payload, json_payload, raw_payload


@dataclass(frozen=True)
class Greeting:
    name: str
    count: int


def test_json_payload_round_trips_value() -> None:
    value = {"name": "aion", "count": 2, "ok": True}
    payload = json_payload(value)
    assert payload.content_type == "application/json"
    assert decode_payload(payload) == value


def test_json_payload_round_trips_dataclass_model() -> None:
    payload = json_payload({"name": "aion", "count": 2})
    assert decode_payload(payload, Greeting) == Greeting(name="aion", count=2)


def test_raw_payload_escape_hatch_keeps_bytes() -> None:
    payload = raw_payload(b"\x00\x01", "application/octet-stream")
    assert decode_payload(payload) == b"\x00\x01"


def test_non_serialisable_payload_raises_invalid_argument() -> None:
    value = cast(JSONValue, {"bad": object()})
    with pytest.raises(InvalidArgument):
        json_payload(value)
