"""Live conformance harness for the shared Aion client scenarios."""
from __future__ import annotations

import asyncio
import json
import os
from collections.abc import AsyncIterator, Mapping, Sequence
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any, Protocol
from urllib.parse import urlparse

import pytest

from aion_client import Client
from aion_client.errors import AionClientError, InvalidArgument, Unavailable
from aion_client.stream import EventStream
from aion_client.transport import default_events_endpoint

SERVER_URL_ENV = "AION_SERVER_URL"
AUTH_TOKEN_ENV = "AION_AUTH_TOKEN"
JSONValue = None | bool | int | float | str | list["JSONValue"] | dict[str, "JSONValue"]
Observable = dict[str, JSONValue]


class _ClosableEventStream(Protocol):
    """The slice of EventStream the collector needs (iterate + aclose)."""

    def __aiter__(self) -> AsyncIterator[Any]: ...
    async def aclose(self) -> None: ...


@dataclass(slots=True, frozen=True)
class CollectPlan:
    """Collector thresholds taken from the subscribe step's collect block.

    An absent minimum means no minimum (zero), and the timeout comes from the
    step's own timeoutMs.
    """

    minimum_events_before_disconnect: int
    minimum_events_after_reconnect: int
    timeout_ms: float


class _RelayLink:
    """One client<->upstream TCP connection pair piped through the relay."""

    def __init__(
        self,
        client_reader: asyncio.StreamReader,
        client_writer: asyncio.StreamWriter,
        upstream_reader: asyncio.StreamReader,
        upstream_writer: asyncio.StreamWriter,
    ) -> None:
        self._writers = (client_writer, upstream_writer)
        self._tasks = (
            asyncio.ensure_future(self._pump(client_reader, upstream_writer)),
            asyncio.ensure_future(self._pump(upstream_reader, client_writer)),
        )

    async def _pump(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        try:
            while True:
                chunk = await reader.read(65536)
                if not chunk:
                    break
                writer.write(chunk)
                await writer.drain()
        except OSError:
            # A reset peer ends the link; aborting both ends below IS the
            # handling (the other peer observes the drop and reacts).
            pass
        finally:
            self.abort()

    def abort(self) -> None:
        """Abruptly close both ends, leaving nothing half-open."""

        for writer in self._writers:
            writer.transport.abort()

    async def wait_closed(self) -> None:
        results = await asyncio.gather(*self._tasks, return_exceptions=True)
        for result in results:
            if isinstance(result, BaseException) and not isinstance(result, asyncio.CancelledError):
                raise result


class StreamRelay:
    """Local TCP relay between the SDK's stream transport and the server.

    Listens on 127.0.0.1:0 and pipes raw bytes to the target host/port.
    ``force_disconnect`` aborts every currently piped connection while the
    listener stays up, so the SDK's resume reconnect succeeds through the
    same relay endpoint.
    """

    def __init__(self, target_host: str, target_port: int) -> None:
        self._target_host = target_host
        self._target_port = target_port
        self._server: asyncio.Server | None = None
        self._links: set[_RelayLink] = set()

    async def start(self) -> None:
        self._server = await asyncio.start_server(self._accept, host="127.0.0.1", port=0)

    @property
    def port(self) -> int:
        if self._server is None or not self._server.sockets:
            raise InvalidArgument("stream relay has not been started")
        return int(self._server.sockets[0].getsockname()[1])

    async def _accept(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        try:
            upstream_reader, upstream_writer = await asyncio.open_connection(self._target_host, self._target_port)
        except OSError as exc:
            # The SDK observes a dropped socket and retries; the upstream
            # failure itself is reported so it is never silent.
            print(f"AION_CONFORMANCE relay upstream connect to {self._target_host}:{self._target_port} failed: {exc}")
            writer.transport.abort()
            return
        link = _RelayLink(reader, writer, upstream_reader, upstream_writer)
        self._links.add(link)
        try:
            await link.wait_closed()
        finally:
            self._links.discard(link)

    def force_disconnect(self) -> int:
        """Abort all currently piped connections; the listener stays alive."""

        links = list(self._links)
        for link in links:
            link.abort()
        return len(links)

    async def close(self) -> None:
        if self._server is not None:
            self._server.close()
            await self._server.wait_closed()
            self._server = None
        for link in list(self._links):
            link.abort()
            await link.wait_closed()


class StreamCollector:
    """Single background consumer of the subscribe step's event stream.

    The subscribe step starts exactly one collector; ``harness.assertStream``
    awaits and asserts over THIS collector's accumulated list (one stream
    object spanning the forced disconnect), never a new subscription.
    """

    def __init__(self, stream: _ClosableEventStream) -> None:
        self._stream = stream
        self.events: list[Observable] = []
        self.disconnect_mark: int | None = None
        self.failure: AionClientError | None = None
        self._changed = asyncio.Event()
        self._task = asyncio.ensure_future(self._consume())

    async def _consume(self) -> None:
        try:
            async for event in self._stream:
                self.events.append(_normalize_event(event))
                self._changed.set()
        except AionClientError as exc:
            self.failure = exc
        finally:
            self._changed.set()

    def mark_disconnect(self) -> None:
        """Snapshot how many events were delivered before the forced drop."""

        self.disconnect_mark = len(self.events)

    async def wait_for_total(self, target: int, timeout_s: float) -> bool:
        """Wait until ``target`` events have accumulated; False on timeout.

        A terminal stream failure observed while still short of ``target`` is
        raised: the stream can never reach the target, so pretending to wait
        would hide the failure.
        """

        loop = asyncio.get_running_loop()
        deadline = loop.time() + timeout_s
        while len(self.events) < target:
            if self.failure is not None:
                raise self.failure
            if self._task.done():
                task_error = self._task.exception()
                if task_error is not None:
                    raise task_error
                return False
            remaining = deadline - loop.time()
            if remaining <= 0:
                return False
            self._changed.clear()
            try:
                await asyncio.wait_for(self._changed.wait(), remaining)
            except asyncio.TimeoutError:
                return False
        return True

    async def aclose(self) -> None:
        self._task.cancel()
        try:
            await self._task
        except asyncio.CancelledError:
            pass
        await self._stream.aclose()


@dataclass(slots=True)
class ScenarioContext:
    """Per-scenario execution state and observable results."""
    client: Client | None = None
    relay: StreamRelay | None = None
    collector: StreamCollector | None = None
    collect_plan: CollectPlan | None = None
    results: dict[str, Observable] = field(default_factory=dict)
    error_identities: dict[str, Observable] = field(default_factory=dict)
    def require_client(self) -> Client:
        if self.client is None:
            raise InvalidArgument("scenario has not connected yet")
        return self.client
    def require_collector(self) -> tuple[StreamCollector, CollectPlan]:
        if self.collector is None or self.collect_plan is None:
            raise InvalidArgument("scenario has no active stream collector; run a subscribe step with collect first")
        return self.collector, self.collect_plan
    def record(self, scenario: str, step: str, value: Observable) -> None:
        self.results[f"{scenario}.{step}"] = value
    def record_error_identity(self, scenario: str, step: str, identity: Observable) -> None:
        self.error_identities[f"{scenario}.{step}"] = identity
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
    try:
        for step_value in _list(scenario["steps"]):
            step = _mapping(step_value)
            step_id = str(step["id"])
            operation = str(step["operation"])
            input_value = _resolve(step.get("input", None), defaults, fixtures, scenario_id, context, started_at, now)
            expected = _mapping(_resolve(step["expect"], defaults, fixtures, scenario_id, context, started_at, now))
            error_identity: Observable | None = None
            try:
                ok = await _execute(operation, _mapping(input_value), context, server_url)
                actual: Observable = {"ok": ok}
            except AionClientError as exc:
                actual = {"error": exc.code.value}
                error_identity = {"code": exc.code.value, "message": str(exc), "detail": exc.detail}
            print(
                "AION_CONFORMANCE "
                f"sdk=python scenario={scenario_id} step={step_id} "
                f"result={json.dumps(actual, sort_keys=True, separators=(',', ':'))}"
            )
            _assert_matches(scenario_id, step_id, actual, expected, context, error_identity)
            context.record(scenario_id, step_id, actual)
            if error_identity is not None:
                context.record_error_identity(scenario_id, step_id, error_identity)
    finally:
        await _teardown(context)
async def _teardown(context: ScenarioContext) -> None:
    """Release the scenario's collector, relay, and client.

    Every resource is released even when an earlier release fails; the first
    failure is re-raised afterwards (never swallowed).
    """
    failures: list[Exception] = []
    collector, context.collector = context.collector, None
    if collector is not None:
        try:
            await collector.aclose()
        except Exception as exc:
            failures.append(exc)
    relay, context.relay = context.relay, None
    if relay is not None:
        try:
            await relay.close()
        except Exception as exc:
            failures.append(exc)
    client, context.client = context.client, None
    if client is not None:
        try:
            await client.close()
        except Exception as exc:
            failures.append(exc)
    if failures:
        raise failures[0]
async def _execute(
    operation: str,
    input_value: Mapping[str, Any],
    context: ScenarioContext,
    server_url: str,
) -> Observable:
    if operation == "connect":
        tls = server_url.startswith("https://")
        events_endpoint = await _relayed_events_endpoint(context, server_url, tls)
        context.client = await Client.connect(
            server_url,
            auth=os.environ.get(AUTH_TOKEN_ENV) or None,
            tls=tls,
            namespace=str(input_value.get("namespace", "conformance")),
            events_endpoint=events_endpoint,
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
            # ONE background collector; its stream object and accumulated
            # delivery list live in the scenario context so the later
            # harness steps act on the same stream across the disconnect.
            stream: EventStream[Any] = context.require_client().handle(_selector_workflow_id(input_value)).subscribe()
            context.collector = StreamCollector(stream)
            context.collect_plan = _collect_plan(input_value)
            return {"kind": "eventStreamStarted"}
        collected = await _collect_stream(input_value, context)
        return {"kind": "eventStream", "events": _events_json(collected)}
    if operation == "harness.forceDisconnect":
        return await _force_disconnect(context)
    if operation == "harness.assertStream":
        return await _assert_stream(input_value, context)
    raise InvalidArgument(f"unsupported conformance operation {operation}")
async def _relayed_events_endpoint(context: ScenarioContext, server_url: str, tls: bool) -> str | None:
    """Start the scenario's TCP relay and return the relayed ws endpoint.

    The SDK's stream endpoint is pointed at a local relay that pipes bytes to
    the real server, so ``harness.forceDisconnect`` can sever live stream
    connections without touching the server. TLS stream endpoints are not
    relayed (a raw byte pipe cannot satisfy certificate validation for the
    original host); forceDisconnect then refuses with a precise error
    instead of pretending to disconnect.
    """
    if context.relay is not None:
        await context.relay.close()
        context.relay = None
    direct = urlparse(default_events_endpoint(server_url, tls))
    if direct.scheme != "ws" or direct.hostname is None:
        return None
    relay = StreamRelay(direct.hostname, direct.port if direct.port is not None else 80)
    await relay.start()
    context.relay = relay
    return f"ws://127.0.0.1:{relay.port}{direct.path}"
async def _force_disconnect(context: ScenarioContext) -> Observable:
    """Sever the live piped stream sockets after the before-disconnect minimum."""
    collector, plan = context.require_collector()
    relay = context.relay
    if relay is None:
        raise InvalidArgument(
            "harness.forceDisconnect requires the plaintext ws:// stream relay; "
            "wss stream endpoints cannot be byte-relayed"
        )
    reached = await collector.wait_for_total(plan.minimum_events_before_disconnect, plan.timeout_ms / 1000.0)
    if not reached:
        raise Unavailable(
            f"stream delivered {len(collector.events)} events within {plan.timeout_ms}ms; "
            f"needed {plan.minimum_events_before_disconnect} before the forced disconnect"
        )
    collector.mark_disconnect()
    dropped = relay.force_disconnect()
    if dropped == 0:
        raise Unavailable("no live stream connection was piped through the relay to disconnect")
    return {"kind": "disconnectInjected", "droppedConnections": dropped}
async def _assert_stream(input_value: Mapping[str, Any], context: ScenarioContext) -> Observable:
    """Await the subscribe step's collector and report its accumulated list."""
    collector, plan = context.require_collector()
    target = plan.minimum_events_before_disconnect + plan.minimum_events_after_reconnect
    if collector.disconnect_mark is not None:
        target = max(target, collector.disconnect_mark + plan.minimum_events_after_reconnect)
    # A timeout returns the partial list; the step's expectations then fail
    # with the accumulated evidence instead of the harness hanging.
    await collector.wait_for_total(target, _timeout_ms(input_value) / 1000.0)
    events = list(collector.events)
    return {
        "kind": "eventStream",
        "events": _events_json(events),
        "sequenceContiguousUnique": _sequences_contiguous_unique(events),
    }
def _selector_workflow_id(input_value: Mapping[str, Any]) -> str:
    selector = _mapping(input_value.get("selector", input_value))
    return str(selector.get("workflowId", input_value.get("workflowId", "")))
def _collect_plan(input_value: Mapping[str, Any]) -> CollectPlan:
    collect = _mapping(input_value.get("collect"))
    return CollectPlan(
        minimum_events_before_disconnect=int(collect.get("minimumEventsBeforeDisconnect", 0)),
        minimum_events_after_reconnect=int(collect.get("minimumEventsAfterReconnect", 0)),
        timeout_ms=_timeout_ms(input_value),
    )
def _events_json(events: list[Observable]) -> list[JSONValue]:
    json_events: list[JSONValue] = []
    json_events.extend(events)
    return json_events
async def _collect_stream(input_value: Mapping[str, Any], context: ScenarioContext) -> list[Observable]:
    handle = context.require_client().handle(_selector_workflow_id(input_value))
    timeout_ms = _timeout_ms(input_value)
    wanted = [str(item) for item in _list(_mapping(input_value.get("collectUntil", {})).get("eventTypes", []))]
    events: list[Observable] = []
    stream: EventStream[Any] = handle.subscribe()
    async def _collect() -> None:
        async for event in stream:
            events.append(_normalize_event(event))
            if wanted and all(any(item.get("type") == kind for item in events) for kind in wanted):
                return
            if not wanted and len(events) >= 3:
                return
    try:
        await asyncio.wait_for(_collect(), timeout_ms / 1000.0)
    except asyncio.TimeoutError:
        # Partial collection is asserted by the step's expectations.
        return events
    finally:
        await stream.aclose()
    return events
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
    error_identity: Observable | None,
) -> None:
    detail = f"scenario={scenario} step={step} expected={expected} actual={actual}"
    if "error" in expected:
        assert actual.get("error") == expected["error"], detail
        if "errorSameAs" in expected:
            reference = context.error_identities.get(str(expected["errorSameAs"]))
            assert reference is not None, detail
            assert error_identity is not None, detail
            assert error_identity == reference, (
                f"{detail} error_identity={error_identity} reference={reference}"
            )
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


# --- Offline unit tests for the harness's pure plumbing -------------------
#
# The live conformance run above is gated on AION_SERVER_URL; the relay and
# collector mechanics are exercised here without any server so the harness
# code is verified real, not assumed.


async def _start_echo_target() -> tuple[asyncio.Server, int]:
    """A local TCP echo server standing in for the live aion-server."""

    async def _serve(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        try:
            while True:
                chunk = await reader.read(1024)
                if not chunk:
                    break
                writer.write(chunk)
                await writer.drain()
        except OSError:
            # The relay aborting the pipe mid-echo is the scripted outcome.
            pass
        finally:
            writer.close()

    server = await asyncio.start_server(_serve, host="127.0.0.1", port=0)
    return server, int(server.sockets[0].getsockname()[1])


async def _read_eof(reader: asyncio.StreamReader) -> bytes:
    """Read the peer-drop outcome: EOF and reset are both a dropped socket."""

    try:
        return await asyncio.wait_for(reader.read(1024), 5.0)
    except ConnectionResetError:
        return b""


@pytest.mark.asyncio
async def test_relay_pipes_force_disconnects_and_accepts_reconnects() -> None:
    """forceDisconnect drops the piped sockets but keeps the listener: a new
    connection through the SAME relay endpoint pipes bytes again, which is
    exactly what the SDK's resume reconnect needs."""

    target, target_port = await _start_echo_target()
    relay = StreamRelay("127.0.0.1", target_port)
    await relay.start()
    try:
        reader, writer = await asyncio.open_connection("127.0.0.1", relay.port)
        writer.write(b"before")
        await writer.drain()
        assert await asyncio.wait_for(reader.readexactly(6), 5.0) == b"before"

        assert relay.force_disconnect() == 1
        assert await _read_eof(reader) == b""
        writer.close()

        resumed_reader, resumed_writer = await asyncio.open_connection("127.0.0.1", relay.port)
        resumed_writer.write(b"after")
        await resumed_writer.drain()
        assert await asyncio.wait_for(resumed_reader.readexactly(5), 5.0) == b"after"
        resumed_writer.close()
    finally:
        await relay.close()
        target.close()
        await target.wait_closed()


@pytest.mark.asyncio
async def test_relay_force_disconnect_without_connections_drops_nothing() -> None:
    """With no piped sockets there is nothing to drop and the relay says so
    (the harness then refuses to report disconnectInjected)."""

    target, target_port = await _start_echo_target()
    relay = StreamRelay("127.0.0.1", target_port)
    await relay.start()
    try:
        assert relay.force_disconnect() == 0
    finally:
        await relay.close()
        target.close()
        await target.wait_closed()


def test_relay_port_before_start_is_refused() -> None:
    with pytest.raises(InvalidArgument):
        _ = StreamRelay("127.0.0.1", 1).port


@pytest.mark.asyncio
async def test_relayed_events_endpoint_points_sdk_at_local_relay() -> None:
    """connect rewires the SDK's stream endpoint through the local relay,
    preserving the /events/stream path."""

    context = ScenarioContext()
    try:
        endpoint = await _relayed_events_endpoint(context, "http://127.0.0.1:9", False)
        assert context.relay is not None
        assert endpoint == f"ws://127.0.0.1:{context.relay.port}/events/stream"
    finally:
        await _teardown(context)


@pytest.mark.asyncio
async def test_relayed_events_endpoint_refuses_tls() -> None:
    """wss endpoints are not byte-relayed: no relay, default endpoint kept."""

    context = ScenarioContext()
    try:
        assert await _relayed_events_endpoint(context, "https://aion.example", True) is None
        assert context.relay is None
    finally:
        await _teardown(context)


def test_collect_plan_reads_minimums_and_timeout() -> None:
    plan = _collect_plan(
        {
            "collect": {
                "minimumEventsBeforeDisconnect": 1,
                "minimumEventsAfterReconnect": 2,
                "timeoutMs": 15000,
            }
        }
    )
    assert plan == CollectPlan(
        minimum_events_before_disconnect=1,
        minimum_events_after_reconnect=2,
        timeout_ms=15000.0,
    )


class _ScriptedStream:
    """Stands in for the subscribe step's EventStream in collector tests."""

    def __init__(self) -> None:
        self._queue: asyncio.Queue[dict[str, JSONValue] | BaseException | None] = asyncio.Queue()
        self.closed = False

    def feed(self, seq: int, event_type: str = "WorkflowSignalled") -> None:
        self._queue.put_nowait({"type": event_type, "seq": seq})

    def feed_failure(self, failure: BaseException) -> None:
        self._queue.put_nowait(failure)

    def finish(self) -> None:
        self._queue.put_nowait(None)

    def __aiter__(self) -> AsyncIterator[Any]:
        return self._iterate()

    async def _iterate(self) -> AsyncIterator[Any]:
        while True:
            item = await self._queue.get()
            if item is None:
                return
            if isinstance(item, BaseException):
                raise item
            yield item

    async def aclose(self) -> None:
        self.closed = True


@pytest.mark.asyncio
async def test_stream_collector_accumulates_one_list_across_disconnect_mark() -> None:
    """One collector, one accumulated list: the disconnect mark plus the
    after-reconnect minimum gate assertStream's wait, and the contiguity
    check runs over the WHOLE list spanning the disconnect."""

    stream = _ScriptedStream()
    collector = StreamCollector(stream)
    try:
        stream.feed(1)
        assert await collector.wait_for_total(1, 5.0) is True
        collector.mark_disconnect()
        assert collector.disconnect_mark == 1

        stream.feed(2)
        stream.feed(3)
        assert await collector.wait_for_total(collector.disconnect_mark + 2, 5.0) is True
        assert [event["seq"] for event in collector.events] == [1, 2, 3]
        assert _sequences_contiguous_unique(collector.events) is True
    finally:
        await collector.aclose()
    assert stream.closed is True


@pytest.mark.asyncio
async def test_stream_collector_wait_times_out_short_of_target() -> None:
    stream = _ScriptedStream()
    collector = StreamCollector(stream)
    try:
        stream.feed(1)
        assert await collector.wait_for_total(2, 0.05) is False
        assert [event["seq"] for event in collector.events] == [1]
    finally:
        await collector.aclose()


@pytest.mark.asyncio
async def test_stream_collector_surfaces_terminal_stream_failure() -> None:
    """A terminal stream failure short of the target is raised, never waited
    out: a broken resume must fail the scenario loudly."""

    stream = _ScriptedStream()
    collector = StreamCollector(stream)
    try:
        stream.feed(1)
        stream.feed_failure(Unavailable("resume failed"))
        with pytest.raises(Unavailable, match="resume failed"):
            await collector.wait_for_total(2, 5.0)
    finally:
        await collector.aclose()


@pytest.mark.asyncio
async def test_stream_collector_clean_stream_end_short_of_target_returns_false() -> None:
    stream = _ScriptedStream()
    collector = StreamCollector(stream)
    try:
        stream.feed(1)
        stream.finish()
        assert await collector.wait_for_total(3, 5.0) is False
        assert [event["seq"] for event in collector.events] == [1]
    finally:
        await collector.aclose()
