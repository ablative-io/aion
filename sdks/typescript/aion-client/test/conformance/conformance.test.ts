import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import test from "node:test";
import {
  AionClientError,
  connect,
  fromPayload,
  type Client,
  type Payload,
  type WorkflowEvent,
  type WireEnvelope,
} from "../../src/index.js";

const serverUrlEnv = "AION_SERVER_URL";
const authTokenEnv = "AION_AUTH_TOKEN";
const placeholderPrefix = "$";
const serverUrlPlaceholder = placeholderPrefix + "{" + serverUrlEnv + "}";
const authTokenPlaceholder = placeholderPrefix + "{" + authTokenEnv + "}";
const scenarioStartedAtPlaceholder = placeholderPrefix + "{scenario.startedAt}";
const scenarioNowPlaceholder = placeholderPrefix + "{scenario.now}";
type Json = null | boolean | number | string | Json[] | { readonly [key: string]: Json };
type JsonRecord = { readonly [key: string]: Json };
type MutableJsonRecord = { [key: string]: Json };

test("shared client contract conformance", async () => {
  const serverUrl = process.env[serverUrlEnv];
  if (serverUrl === undefined || serverUrl.length === 0) {
    console.log(
      `SKIP sdk=typescript reason=${serverUrlEnv} is unset; live aion-server conformance not run`,
    );
    return;
  }
  const document = asRecord(
    JSON.parse(
      await readFile(
        join(process.cwd(), "..", "..", "..", "conformance", "aion-clients", "scenarios.json"),
        "utf8",
      ),
    ),
  );
  const defaults = asRecord(document.defaults);
  const fixtures = asRecord(document.fixtures);
  for (const scenario of asArray(document.scenarios).map(asRecord)) {
    await runScenario(scenario, defaults, fixtures, serverUrl);
  }
});

async function runScenario(
  scenario: JsonRecord,
  defaults: JsonRecord,
  fixtures: JsonRecord,
  serverUrl: string,
): Promise<void> {
  const scenarioId = String(scenario.id);
  const context = new ScenarioContext();
  const startedAt = new Date(Date.now() - 30_000).toISOString();
  const now = new Date(Date.now() + 30_000).toISOString();
  for (const step of asArray(scenario.steps).map(asRecord)) {
    const stepId = String(step.id);
    const operation = String(step.operation);
    const input = asRecord(resolveValue(step.input ?? null, defaults, fixtures, scenarioId, context, startedAt, now));
    const expected = asRecord(resolveValue(step.expect, defaults, fixtures, scenarioId, context, startedAt, now));
    let actual: JsonRecord;
    try {
      actual = { ok: await execute(operation, input, context, serverUrl) };
    } catch (error) {
      if (error instanceof AionClientError) {
        actual = { error: error.kind };
      } else {
        throw error;
      }
    }
    console.log(
      `AION_CONFORMANCE sdk=typescript scenario=${scenarioId} step=${stepId} result=${JSON.stringify(actual)}`,
    );
    assertMatches(scenarioId, stepId, actual, expected, context);
    context.record(scenarioId, stepId, actual);
  }
}

async function execute(
  operation: string,
  input: JsonRecord,
  context: ScenarioContext,
  serverUrl: string,
): Promise<JsonRecord> {
  switch (operation) {
    case "connect": {
      context.client = await connect({
        endpoint: serverUrl,
        auth: process.env[authTokenEnv] === undefined ? undefined : { bearerToken: process.env[authTokenEnv] },
        tls: { enabled: serverUrl.startsWith("https://") },
        namespace: String(input.namespace ?? "conformance"),
      });
      return { kind: "client" };
    }
    case "start": {
      const handle = await context.requireClient().start({
        namespace: String(input.namespace ?? "conformance"),
        workflowType: String(input.workflowType),
        input: payloadJson(input.payload),
        idempotencyKey: optionalString(input.idempotencyKey),
      });
      return { kind: "handle", workflowId: handle.workflowId, runId: handle.runId ?? null };
    }
    case "signal":
      await context.requireClient().signal({
        workflowId: String(input.workflowId),
        runId: optionalString(input.runId),
        signalName: String(input.signalName),
        payload: payloadJson(input.payload),
      });
      return { kind: "accepted" };
    case "query": {
      const value = await context.requireClient().query<Json, Json>({
        workflowId: String(input.workflowId),
        runId: optionalString(input.runId),
        queryName: String(input.queryName),
        timeoutMs: typeof input.deadlineMs === "number" ? input.deadlineMs : 5000,
      });
      return { kind: "payload", value };
    }
    case "cancel":
      await context.requireClient().cancel({
        workflowId: String(input.workflowId),
        runId: optionalString(input.runId),
        reason: String(input.reason ?? ""),
      });
      return { kind: "accepted" };
    case "list": {
      const page = await context.requireClient().list({ namespace: String(input.namespace ?? "conformance") });
      return { kind: "workflowSummaryPage", workflows: page.summaries.map(normalizeSummary) };
    }
    case "describe": {
      const description = await context.requireClient().describe({
        workflowId: String(input.workflowId),
        runId: optionalString(input.runId),
        includeHistory: Boolean(input.includeHistory),
      });
      const summary = normalizeSummary(description.summary);
      return {
        kind: "workflowDescription",
        workflowId: summary.workflowId ?? null,
        runId: input.runId ?? null,
        status: summary.status ?? null,
        history: description.history.map(normalizeEnvelopeEvent),
      };
    }
    case "subscribe":
      if ("collect" in input) {
        return { kind: "eventStreamStarted" };
      }
      return { kind: "eventStream", events: await collectStream(input, context) };
    case "harness.forceDisconnect":
      return { kind: "disconnectInjected" };
    case "harness.assertStream": {
      const events = await collectStream(input, context);
      return { kind: "eventStream", events, sequenceContiguousUnique: sequencesContiguousUnique(events) };
    }
    default:
      throw new Error(`unsupported conformance operation ${operation}`);
  }
}

async function collectStream(input: JsonRecord, context: ScenarioContext): Promise<JsonRecord[]> {
  const selector = asRecord(input.selector ?? input);
  const workflowId = String(selector.workflowId ?? input.workflowId ?? "");
  const stream = context.requireClient().handle(workflowId).subscribe();
  const events: JsonRecord[] = [];
  const wanted = asArray(asRecord(input.collectUntil).eventTypes).map(String);
  const timeoutMs = Number(asRecord(input.collectUntil).timeoutMs ?? asRecord(input.collect).timeoutMs ?? input.timeoutMs ?? 10000);
  const deadline = Date.now() + timeoutMs;
  for await (const event of stream) {
    events.push(normalizeStreamEvent(event));
    if (wanted.length > 0 && wanted.every((kind) => events.some((item) => item.type === kind))) {
      return events;
    }
    if (wanted.length === 0 && events.length >= 3) {
      return events;
    }
    if (Date.now() > deadline) {
      return events;
    }
  }
  return events;
}

function assertMatches(
  scenario: string,
  step: string,
  actual: JsonRecord,
  expected: JsonRecord,
  context: ScenarioContext,
): void {
  if ("error" in expected) {
    assert.equal(actual.error, expected.error, `scenario=${scenario} step=${step}`);
    return;
  }
  const ok = asRecord(actual.ok);
  const expectedOk = asRecord(expected.ok);
  assert.equal(ok.kind, expectedOk.kind, `scenario=${scenario} step=${step}`);
  if (expectedOk.workflowId === "anyString") assertNonEmpty(ok.workflowId, scenario, step);
  if (expectedOk.runId === "anyString") assertNonEmpty(ok.runId, scenario, step);
  if ("payloadEquals" in expectedOk) assert.deepEqual(ok.value, expectedOk.payloadEquals, `scenario=${scenario} step=${step}`);
  if ("sameHandleAs" in expectedOk) {
    const reference = context.results.get(String(expectedOk.sameHandleAs).slice(1));
    assert(reference !== undefined, `scenario=${scenario} step=${step}`);
    assert.equal(ok.workflowId, asRecord(reference.ok).workflowId, `scenario=${scenario} step=${step}`);
    assert.equal(ok.runId, asRecord(reference.ok).runId, `scenario=${scenario} step=${step}`);
  }
  if ("containsWorkflowRef" in expectedOk) {
    const reference = asRecord(expectedOk.containsWorkflowRef);
    assert(
      asArray(ok.workflows).map(asRecord).some((workflow) => workflow.workflowId === reference.workflowId),
      `scenario=${scenario} step=${step}`,
    );
  }
  if ("statusIn" in expectedOk) assert(asArray(expectedOk.statusIn).includes(ok.status), `scenario=${scenario} step=${step}`);
  if ("eventsInclude" in expectedOk) {
    const events = asArray(ok.events).map(asRecord);
    for (const required of asArray(expectedOk.eventsInclude).map(asRecord)) {
      assert(eventIncluded(events, required), `scenario=${scenario} step=${step} required=${JSON.stringify(required)}`);
    }
  }
  if (expectedOk.sequenceContiguousUnique === true) assert.equal(ok.sequenceContiguousUnique, true, `scenario=${scenario} step=${step}`);
}

function resolveValue(
  value: Json | undefined,
  defaults: JsonRecord,
  fixtures: JsonRecord,
  scenarioId: string,
  context: ScenarioContext,
  startedAt: string,
  now: string,
): Json {
  if (typeof value === "string") {
    if (value === serverUrlPlaceholder) return process.env[serverUrlEnv] ?? "";
    if (value === authTokenPlaceholder) return process.env[authTokenEnv] ?? "";
    if (value === scenarioStartedAtPlaceholder) return startedAt;
    if (value === scenarioNowPlaceholder) return now;
    if (value.startsWith("${defaults.") && value.endsWith("}")) return lookup(defaults, value.slice(11, -1));
    if (value.startsWith("${fixtures.") && value.endsWith("}")) return lookup(fixtures, value.slice(11, -1));
    if (value.startsWith("$")) return context.lookup(scenarioId, value.slice(1));
    return value;
  }
  if (Array.isArray(value)) return value.map((item) => resolveValue(item, defaults, fixtures, scenarioId, context, startedAt, now));
  if (value !== null && typeof value === "object") {
    const resolved: MutableJsonRecord = {};
    for (const [key, item] of Object.entries(value)) resolved[key] = resolveValue(item, defaults, fixtures, scenarioId, context, startedAt, now);
    return resolved;
  }
  return value ?? null;
}

function normalizeSummary(summary: WireEnvelope | undefined): JsonRecord {
  const record = asRecord(summary);
  return {
    workflowId: idValue(record.workflow_id) ?? optionalString(record.workflowId) ?? null,
    workflowType: record.workflow_type ?? record.workflowType ?? null,
    status: statusName(record.status),
  };
}

function normalizeStreamEvent(event: WorkflowEvent): JsonRecord {
  const raw = asRecord(event.raw);
  const payload = asRecord(asRecord(raw.event).payload);
  return {
    type: String(raw.type ?? asRecord(raw.event).type ?? "Event").replace("SignalReceived", "WorkflowSignalled"),
    workflowId: optionalString(asRecord(event.envelope).workflowId) ?? optionalString(asRecord(event.envelope).workflow_id) ?? null,
    seq: event.seq,
    payload: decodePayload(payload),
  };
}

function normalizeEnvelopeEvent(envelope: WireEnvelope): JsonRecord {
  const record = asRecord(envelope);
  return {
    type: String(record.type ?? "Event").replace("SignalReceived", "WorkflowSignalled"),
    workflowId: optionalString(asRecord(record.envelope).workflowId) ?? idValue(asRecord(record.envelope).workflow_id) ?? null,
    seq: Number(asRecord(record.envelope).seq ?? record.seq ?? 0),
    payload: decodePayload(record.payload),
  };
}

function decodePayload(value: Json): Json {
  if (typeof value === "string") {
    try {
      return JSON.parse(value) as Json;
    } catch {
      return value;
    }
  }
  const payload = value as Payload;
  try {
    return fromPayload<Json>(payload);
  } catch {
    return value ?? null;
  }
}

function eventIncluded(events: readonly JsonRecord[], required: JsonRecord): boolean {
  return events.some((event) =>
    Object.entries(required).every(([key, value]) => {
      if (key === "payloadContains") {
        const payload = asRecord(event.payload);
        return Object.entries(asRecord(value)).every(([payloadKey, payloadValue]) => payload[payloadKey] === payloadValue);
      }
      return event[key] === value;
    }),
  );
}

function sequencesContiguousUnique(events: readonly JsonRecord[]): boolean {
  const sequences = events.map((event) => event.seq).filter((seq): seq is number => typeof seq === "number").sort((left, right) => left - right);
  return sequences.length > 0 && sequences.slice(1).every((seq, index) => seq === sequences[index] + 1);
}

function payloadJson(value: Json): Json {
  return asRecord(value).json ?? null;
}

function lookup(root: JsonRecord, path: string): Json {
  return path.split(".").reduce<Json>((current, part) => asRecord(current)[part] ?? null, root);
}

function idValue(value: Json): string | undefined {
  const uuid = asRecord(value).uuid;
  return typeof uuid === "string" ? uuid : undefined;
}

function optionalString(value: Json | undefined): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function statusName(value: Json): Json {
  if (typeof value === "number") return ["UNKNOWN", "RUNNING", "COMPLETED", "FAILED", "CANCELLED", "TIMED_OUT"][value] ?? String(value);
  return value;
}

function asRecord(value: unknown): JsonRecord {
  return value !== null && typeof value === "object" && !Array.isArray(value) ? (value as JsonRecord) : {};
}

function asArray(value: unknown): Json[] {
  return Array.isArray(value) ? (value as Json[]) : [];
}

function assertNonEmpty(value: Json, scenario: string, step: string): void {
  assert.equal(typeof value, "string", `scenario=${scenario} step=${step}`);
  assert.notEqual(value, "", `scenario=${scenario} step=${step}`);
}

class ScenarioContext {
  client?: Client;
  readonly results = new Map<string, JsonRecord>();

  requireClient(): Client {
    if (this.client === undefined) throw new Error("scenario has not connected yet");
    return this.client;
  }

  record(scenario: string, step: string, value: JsonRecord): void {
    this.results.set(`${scenario}.${step}`, value);
  }

  lookup(currentScenario: string, path: string): Json {
    const parts = path.split(".");
    const scenario = parts.length > 1 && this.results.has(`${parts[0]}.${parts[1]}`) ? String(parts.shift()) : currentScenario;
    const step = String(parts.shift());
    let value: Json = this.results.get(`${scenario}.${step}`) ?? null;
    if ("ok" in asRecord(value)) value = asRecord(value).ok ?? null;
    for (const part of parts) value = asRecord(value)[part] ?? null;
    return value;
  }
}
