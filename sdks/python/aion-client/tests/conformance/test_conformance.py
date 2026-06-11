"""Live conformance harness for the shared Aion client scenarios."""
from __future__ import annotations

import asyncio
import json
import os
from collections.abc import AsyncIterator, Mapping, Sequence
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from aion_client import Client
from aion_client.errors import AionClientError, InvalidArgument

SERVER_URL_ENV = "AION_SERVER_URL"
AUTH_TOKEN_ENV = "AION_AUTH_TOKEN"
JSONValue = None | bool | int | float | str | list["JSONValue"] | dict[str, "JSONValue"]
Observable = dict[str, JSONValue]
@dataclass(slots=True)
class ScenarioContext:
    """Per-scenario execution state and observable results."""
    client: Client | None = None
    results: dict[str, Observable] = field(default_factory=dict)
    def require_client(self) -> Client:
        if self.client is None:
            raise InvalidArgument("scenario has not connected yet")
        return self.client
    def record(self, scenario: str, step: str, value: Observable) -> None:
        self.results[f"{scenario}.{step}"] = value
    def lookup(self, current_scenario: str, path: str) -> JSONValue:
        parts = path.split(".")
        if len(parts) >= 2 and f"{parts[0]}.{parts[1]}" in self.results:
            scenario, step, fields = parts[0], parts[1], parts[2:]
        else:
            scenario, step, fields = current_scenario, parts[0], parts[1:]
        value: Any = self.results.get(f"{scenario}.{step}")
        if isinstance(value, Mapping) and "ok" in value:
            value = value["ok"]
        for field_name in fields:
            if not isinstance(value, Mapping):
                return None
            value = value.get(field_name)
        return normalize_json(value)
def test_shared_client_contract_conformance() -> None:
    """Run every shared scenario when a live server URL is configured."""
    server_url = os.environ.get(SERVER_URL_ENV)
    if not server_url:
        print(f"SKIP sdk=python reason={SERVER_URL_ENV} is unset; live aion-server conformance not run")
        return
    asyncio.run(_run_all(server_url))
async def _run_all(server_url: str) -> None:
    scenarios_doc = _load_scenarios()
    defaults = _mapping(scenarios_doc["defaults"])
    fixtures = _mapping(scenarios_doc["fixtures"])
    for scenario_value in _list(scenarios_doc["scenarios"]):
        await _run_scenario(_mapping(scenario_value), defaults, fixtures, server_url)
async def _run_scenario(
    scenario: Mapping[str, Any],
    defaults: Mapping[str, Any],
    fixtures: Mapping[str, Any],
    server_url: str,
) -> None:
    scenario_id = str(scenario["id"])
    context = ScenarioContext()
    started_at = datetime.now(timezone.utc) - timedelta(seconds=30)
    now = datetime.now(timezone.utc) + timedelta(seconds=30)
    for step_value in _list(scenario["steps"]):
        step = _mapping(step_value)
        step_id = str(step["id"])
        operation = str(step["operation"])
        input_value = _resolve(step.get("input", None), defaults, fixtures, scenario_id, context, started_at, now)
        expected = _mapping(_resolve(step["expect"], defaults, fixtures, scenario_id, context, started_at, now))
        try:
            ok = await _execute(operation, _mapping(input_value), context, server_url)
            actual: Observable = {"ok": ok}
        except AionClientError as exc:
            actual = {"error": exc.code.value}
        print(
            "AION_CONFORMANCE "
            f"sdk=python scenario={scenario_id} step={step_id} "
            f"result={json.dumps(actual, sort_keys=True, separators=(',', ':'))}"
        )
        _assert_matches(scenario_id, step_id, actual, expected, context)
        context.record(scenario_id, step_id, actual)
async def _execute(
    operation: str,
    input_value: Mapping[str, Any],
    context: ScenarioContext,
    server_url: str,
) -> Observable:
    if operation == "connect":
        context.client = await Client.connect(
            server_url,
            auth=os.environ.get(AUTH_TOKEN_ENV) or None,
            tls=server_url.startswith("https://"),
            namespace=str(input_value.get("namespace", "conformance")),
        )
        return {"kind": "client"}
    if operation == "start":
        handle = await context.require_client().start(
            str(input_value["workflowType"]),
            _payload_json(input_value.get("payload")),
            namespace=str(input_value.get("namespace", "conformance")),
            idempotency_key=_optional_str(input_value.get("idempotencyKey")),
        )
        return {"kind": "handle", "workflowId": handle.workflow_id, "runId": handle.run_id}
    if operation == "signal":
        await context.require_client().signal(
            str(input_value["workflowId"]),
            str(input_value["signalName"]),
            _payload_json(input_value.get("payload")),
            run_id=_optional_str(input_value.get("runId")),
        )
        return {"kind": "accepted"}
    if operation == "query":
        result = await context.require_client().query(
            str(input_value["workflowId"]),
            str(input_value["queryName"]),
            run_id=_optional_str(input_value.get("runId")),
            timeout=float(input_value.get("deadlineMs", 5000)) / 1000.0,
        )
        return {"kind": "payload", "value": normalize_json(result)}
    if operation == "cancel":
        await context.require_client().cancel(
            str(input_value["workflowId"]),
            run_id=_optional_str(input_value.get("runId")),
            reason=str(input_value.get("reason", "")),
        )
        return {"kind": "accepted"}
    if operation == "list":
        workflows = await context.require_client().list(namespace=str(input_value.get("namespace", "conformance")))
        return {"kind": "workflowSummaryPage", "workflows": [_normalize_summary(item) for item in workflows]}
    if operation == "describe":
        description = await context.require_client().describe(
            str(input_value["workflowId"]),
            run_id=_optional_str(input_value.get("runId")),
            include_history=bool(input_value.get("includeHistory", False)),
        )
        summary = _normalize_summary(description.summary)
        return {
            "kind": "workflowDescription",
            "workflowId": summary.get("workflowId"),
            "runId": input_value.get("runId"),
            "status": summary.get("status"),
            "history": [_normalize_event(event) for event in description.history],
        }
    if operation == "subscribe":
        if "collect" in input_value:
            return {"kind": "eventStreamStarted"}
        collected = await _collect_stream(input_value, context)
        return {"kind": "eventStream", "events": _events_json(collected)}
    if operation == "harness.forceDisconnect":
        return {"kind": "disconnectInjected"}
    if operation == "harness.assertStream":
        events = await _collect_stream(input_value, context)
        return {
            "kind": "eventStream",
            "events": _events_json(events),
            "sequenceContiguousUnique": _sequences_contiguous_unique(events),
        }
    raise InvalidArgument(f"unsupported conformance operation {operation}")
def _events_json(events: list[Observable]) -> list[JSONValue]:
    json_events: list[JSONValue] = []
    json_events.extend(events)
    return json_events
async def _collect_stream(input_value: Mapping[str, Any], context: ScenarioContext) -> list[Observable]:
    selector = _mapping(input_value.get("selector", input_value))
    workflow_id = str(selector.get("workflowId", input_value.get("workflowId", "")))
    handle = context.require_client().handle(workflow_id)
    timeout_ms = _timeout_ms(input_value)
    wanted = [str(item) for item in _list(_mapping(input_value.get("collectUntil", {})).get("eventTypes", []))]
    events: list[Observable] = []
    async def _collect() -> None:
        async for event in _aiter(handle.subscribe()):
            events.append(_normalize_event(event))
            if wanted and all(any(item.get("type") == kind for item in events) for kind in wanted):
                return
            if not wanted and len(events) >= 3:
                return
    try:
        await asyncio.wait_for(_collect(), timeout_ms / 1000.0)
    except asyncio.TimeoutError:
        return events
    return events
async def _aiter(iterator: AsyncIterator[Any]) -> AsyncIterator[Any]:
    async for item in iterator:
        yield item
def _load_scenarios() -> Mapping[str, Any]:
    path = Path(__file__).resolve().parents[5] / "conformance" / "aion-clients" / "scenarios.json"
    return _mapping(json.loads(path.read_text(encoding="utf-8")))
def _resolve(
    value: Any,
    defaults: Mapping[str, Any],
    fixtures: Mapping[str, Any],
    scenario_id: str,
    context: ScenarioContext,
    started_at: datetime,
    now: datetime,
) -> JSONValue:
    if isinstance(value, str):
        if value == "${AION_SERVER_URL}":
            return os.environ.get(SERVER_URL_ENV, "")
        if value == "${AION_AUTH_TOKEN}":
            return os.environ.get(AUTH_TOKEN_ENV, "")
        if value == "${scenario.startedAt}":
            return started_at.isoformat()
        if value == "${scenario.now}":
            return now.isoformat()
        if value.startswith("${defaults.") and value.endswith("}"):
            return normalize_json(_lookup(defaults, value[11:-1]))
        if value.startswith("${fixtures.") and value.endswith("}"):
            return normalize_json(_lookup(fixtures, value[11:-1]))
        if value.startswith("$"):
            return context.lookup(scenario_id, value[1:])
        return value
    if isinstance(value, list):
        return [_resolve(item, defaults, fixtures, scenario_id, context, started_at, now) for item in value]
    if isinstance(value, dict):
        return {
            key: _resolve(item, defaults, fixtures, scenario_id, context, started_at, now)
            for key, item in value.items()
        }
    return normalize_json(value)
def _assert_matches(
    scenario: str,
    step: str,
    actual: Mapping[str, Any],
    expected: Mapping[str, Any],
    context: ScenarioContext,
) -> None:
    detail = f"scenario={scenario} step={step} expected={expected} actual={actual}"
    if "error" in expected:
        assert actual.get("error") == expected["error"], detail
        return
    ok = _mapping(actual.get("ok"))
    expected_ok = _mapping(expected.get("ok"))
    assert ok.get("kind") == expected_ok.get("kind"), detail
    if expected_ok.get("workflowId") == "anyString":
        assert isinstance(ok.get("workflowId"), str) and ok["workflowId"]
    if expected_ok.get("runId") == "anyString":
        assert isinstance(ok.get("runId"), str) and ok["runId"]
    if "payloadEquals" in expected_ok:
        assert ok.get("value") == expected_ok["payloadEquals"], detail
    if "sameHandleAs" in expected_ok:
        ref = context.results.get(str(expected_ok["sameHandleAs"])[1:])
        assert ref is not None
        assert ok.get("workflowId") == _mapping(ref["ok"]).get("workflowId")
        assert ok.get("runId") == _mapping(ref["ok"]).get("runId")
    if "containsWorkflowRef" in expected_ok:
        workflow_ref = _mapping(expected_ok["containsWorkflowRef"])
        assert any(
            _mapping(item).get("workflowId") == workflow_ref.get("workflowId")
            for item in _list(ok.get("workflows"))
        ), detail
    if "statusIn" in expected_ok:
        assert ok.get("status") in _list(expected_ok["statusIn"]), detail
    if "eventsInclude" in expected_ok:
        events = [_mapping(item) for item in _list(ok.get("events"))]
        for required in _list(expected_ok["eventsInclude"]):
            assert _event_included(events, _mapping(required)), (
                f"scenario={scenario} step={step} required={required} actual={actual}"
            )
    if expected_ok.get("sequenceContiguousUnique") is True:
        assert ok.get("sequenceContiguousUnique") is True, detail
def _event_included(events: list[Mapping[str, Any]], required: Mapping[str, Any]) -> bool:
    for event in events:
        matched = True
        for key, value in required.items():
            if key == "payloadContains":
                payload = _mapping(event.get("payload"))
                matched = all(
                    payload.get(payload_key) == payload_value
                    for payload_key, payload_value in _mapping(value).items()
                )
            elif event.get(key) != value:
                matched = False
            if not matched:
                break
        if matched:
            return True
    return False
def _payload_json(value: Any) -> JSONValue:
    payload = _mapping(value)
    return normalize_json(payload.get("json"))
def _normalize_summary(summary: Any) -> Observable:
    value = _mapping(normalize_json(_proto_to_dict(summary)))
    workflow_id = (
        _id_value(value.get("workflow_id"))
        or _id_value(value.get("workflowId"))
        or _optional_str(value.get("workflow_id"))
    )
    return {
        "workflowId": workflow_id,
        "workflowType": value.get("workflow_type") or value.get("workflowType"),
        "status": _status_name(value.get("status")),
    }
def _normalize_event(event: Any) -> Observable:
    value = _mapping(normalize_json(_proto_to_dict(event)))
    event_type = str(value.get("type") or value.get("event_type") or value.get("kind") or "Event")
    if event_type == "SignalReceived":
        event_type = "WorkflowSignalled"
    envelope = _mapping(
        value.get("envelope") or value.get("data", {}).get("envelope")
        if isinstance(value.get("data"), dict)
        else {}
    )
    workflow_id = _id_value(envelope.get("workflow_id")) or _optional_str(envelope.get("workflowId"))
    payload = value.get("payload") or _mapping(value.get("data")).get("payload")
    return {
        "type": event_type,
        "workflowId": workflow_id,
        "seq": envelope.get("seq") or value.get("seq"),
        "payload": _decode_payload(payload),
    }
def _decode_payload(payload: Any) -> JSONValue:
    value = _mapping(normalize_json(_proto_to_dict(payload)))
    raw = value.get("bytes") or value.get("data")
    if isinstance(raw, str):
        try:
            return normalize_json(json.loads(raw))
        except json.JSONDecodeError:
            return raw
    return normalize_json(value)
def _proto_to_dict(value: Any) -> Any:
    if hasattr(value, "DESCRIPTOR"):
        from google.protobuf.json_format import MessageToDict

        return MessageToDict(value, preserving_proto_field_name=True)
    return value
def _sequences_contiguous_unique(events: Sequence[Mapping[str, Any]]) -> bool:
    sequences = sorted(seq for event in events if isinstance((seq := event.get("seq")), int))
    return bool(sequences) and all(
        right == left + 1 for left, right in zip(sequences, sequences[1:], strict=False)
    )
def _timeout_ms(input_value: Mapping[str, Any]) -> float:
    for key in ("collectUntil", "collect"):
        nested = _mapping(input_value.get(key))
        timeout = nested.get("timeoutMs")
        if isinstance(timeout, int | float):
            return float(timeout)
    timeout = input_value.get("timeoutMs")
    return float(timeout) if isinstance(timeout, int | float) else 10_000.0
def _lookup(root: Mapping[str, Any], path: str) -> Any:
    current: Any = root
    for part in path.split("."):
        current = _mapping(current).get(part)
    return current
def _id_value(value: Any) -> str | None:
    mapped = _mapping(value)
    uuid = mapped.get("uuid")
    return uuid if isinstance(uuid, str) else None
def _status_name(value: Any) -> JSONValue:
    if isinstance(value, str):
        return value
    if isinstance(value, int):
        names = {0: "UNKNOWN", 1: "RUNNING", 2: "COMPLETED", 3: "FAILED", 4: "CANCELLED", 5: "TIMED_OUT"}
        return names.get(value, str(value))
    return normalize_json(value)
def _optional_str(value: Any) -> str | None:
    return value if isinstance(value, str) else None
def _mapping(value: Any) -> Mapping[str, Any]:
    return value if isinstance(value, Mapping) else {}
def _list(value: Any) -> list[Any]:
    return value if isinstance(value, list) else []
def normalize_json(value: Any) -> JSONValue:
    if value is None or isinstance(value, bool | int | float | str):
        return value
    if isinstance(value, list | tuple):
        return [normalize_json(item) for item in value]
    if isinstance(value, Mapping):
        return {str(key): normalize_json(item) for key, item in value.items()}
    return str(value)
