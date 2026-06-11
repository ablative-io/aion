import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import net from "node:net";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
	AionClientError,
	CancelledError,
	type Client,
	connect,
	fromPayload,
	InvalidArgumentError,
	type Payload,
	type SubscribeRequest,
	type SubscribeTransport,
	WebSocketSubscribeTransport,
	type WireEnvelope,
	type WorkflowEvent,
} from "../../src/index.js";

const serverUrlEnv = "AION_SERVER_URL";
const authTokenEnv = "AION_AUTH_TOKEN";
const placeholderPrefix = "$";
const serverUrlPlaceholder = `${placeholderPrefix}{${serverUrlEnv}}`;
const authTokenPlaceholder = `${placeholderPrefix}{${authTokenEnv}}`;
const scenarioStartedAtPlaceholder = `${placeholderPrefix}{scenario.startedAt}`;
const scenarioNowPlaceholder = `${placeholderPrefix}{scenario.now}`;
type Json =
	| null
	| boolean
	| number
	| string
	| Json[]
	| { readonly [key: string]: Json };
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
		JSON.parse(await readFile(scenariosPath(), "utf8")),
	);
	const defaults = asRecord(document.defaults);
	const fixtures = asRecord(document.fixtures);
	for (const scenario of asArray(document.scenarios).map(asRecord)) {
		await runScenario(scenario, defaults, fixtures, serverUrl);
	}
});

function scenariosPath(): string {
	// Walk upward from this file (which runs compiled out of dist/) to the
	// repository root that holds the shared scenarios document.
	const relative = join("conformance", "aion-clients", "scenarios.json");
	let directory = dirname(fileURLToPath(import.meta.url));
	while (true) {
		const candidate = join(directory, relative);
		if (existsSync(candidate)) return candidate;
		const parent = dirname(directory);
		if (parent === directory)
			throw new Error(
				`could not locate ${relative} in any ancestor of ${dirname(fileURLToPath(import.meta.url))}`,
			);
		directory = parent;
	}
}

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
	try {
		await runScenarioSteps(
			scenario,
			defaults,
			fixtures,
			serverUrl,
			scenarioId,
			context,
			startedAt,
			now,
		);
	} finally {
		await context.teardown();
	}
}

async function runScenarioSteps(
	scenario: JsonRecord,
	defaults: JsonRecord,
	fixtures: JsonRecord,
	serverUrl: string,
	scenarioId: string,
	context: ScenarioContext,
	startedAt: string,
	now: string,
): Promise<void> {
	for (const step of asArray(scenario.steps).map(asRecord)) {
		const stepId = String(step.id);
		const operation = String(step.operation);
		const input = asRecord(
			resolveValue(
				step.input ?? null,
				defaults,
				fixtures,
				scenarioId,
				context,
				startedAt,
				now,
			),
		);
		const expected = asRecord(
			resolveValue(
				step.expect,
				defaults,
				fixtures,
				scenarioId,
				context,
				startedAt,
				now,
			),
		);
		let actual: JsonRecord;
		let errorIdentity: JsonRecord | null = null;
		try {
			actual = {
				ok: await execute(operation, input, context, serverUrl, expected),
			};
		} catch (error) {
			if (error instanceof AionClientError) {
				actual = { error: error.kind };
				errorIdentity = {
					code: error.kind,
					message: error.message,
					status: error.detail?.status ?? null,
					wireCode: error.detail?.code ?? null,
				};
			} else {
				throw error;
			}
		}
		console.log(
			`AION_CONFORMANCE sdk=typescript scenario=${scenarioId} step=${stepId} result=${JSON.stringify(actual)}`,
		);
		assertMatches(scenarioId, stepId, actual, expected, context, errorIdentity);
		context.record(scenarioId, stepId, actual);
		if (errorIdentity !== null) {
			context.recordErrorIdentity(scenarioId, stepId, errorIdentity);
		}
	}
}

async function execute(
	operation: string,
	input: JsonRecord,
	context: ScenarioContext,
	serverUrl: string,
	expected: JsonRecord,
): Promise<JsonRecord> {
	switch (operation) {
		case "connect": {
			// Caller identity for the server's development-header extraction:
			// the conformance credential holds a grant for `conformance` only
			// — never `conformance-denied` — which is exactly the grant shape
			// the namespace-denied and not-found-anti-leak scenarios pin.
			const bearerToken = process.env[authTokenEnv];
			const auth = {
				...(bearerToken === undefined ? {} : { bearerToken }),
				subject: "conformance-harness",
				namespaces: ["conformance"],
			};
			// Workflow subscriptions ride the built-in WebSocket transport
			// through a local TCP relay so harness.forceDisconnect can sever
			// live stream sockets (and only stream sockets) without touching
			// the server. The relay is transparent TCP, so it only fronts
			// plaintext http endpoints; against https the stream connects
			// directly and forced disconnects are unsupported. The TypeScript
			// client's HTTP endpoint and the WebSocket stream share one
			// listener, so AION_STREAM_URL is an optional override here
			// (required for the gRPC-transport SDKs whose server URL is a
			// different listener).
			const streamUrl = process.env.AION_STREAM_URL ?? serverUrl;
			const target = new URL(streamUrl);
			let streamEndpoint = streamUrl;
			// A repeated connect step (e.g. namespace-denied's connect-denied)
			// replaces the per-scenario stream transport and relay; the
			// previous relay listener must be closed here or its socket
			// outlives the scenario and keeps the test process alive.
			context.streamTransport?.stop();
			if (context.relay !== undefined) {
				await context.relay.close();
				context.relay = undefined;
			}
			if (target.protocol === "http:") {
				const relay = await TcpRelay.start(
					target.hostname,
					target.port.length > 0 ? Number(target.port) : 80,
				);
				context.relay = relay;
				streamEndpoint = `http://127.0.0.1:${relay.port}`;
			}
			const streamTransport = new StoppableSubscribeTransport(
				new WebSocketSubscribeTransport({ endpoint: streamEndpoint, auth }),
			);
			context.streamTransport = streamTransport;
			context.client = await connect({
				endpoint: serverUrl,
				auth,
				tls: { enabled: serverUrl.startsWith("https://") },
				namespace: String(input.namespace ?? "conformance"),
				streamTransport,
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
			return {
				kind: "handle",
				workflowId: handle.workflowId,
				runId: handle.runId ?? null,
			};
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
			const value = await queryWhenRegistered(context, input);
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
			const page = await context
				.requireClient()
				.list({ namespace: String(input.namespace ?? "conformance") });
			return {
				kind: "workflowSummaryPage",
				workflows: page.summaries.map(normalizeSummary),
			};
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
		case "subscribe": {
			if (!("collect" in input)) {
				return {
					kind: "eventStream",
					events: await collectStream(input, context),
				};
			}
			// `collect` marks the disconnect-resume choreography: start the
			// SINGLE background collector that every later harness step
			// (forceDisconnect, assertStream) observes. The forced disconnect
			// needs the relay in front of the stream socket.
			context.requireRelay();
			const selector = asRecord(input.selector ?? input);
			const workflowId = String(
				selector.workflowId ?? input.workflowId ?? "",
			);
			const transport = context.requireStreamTransport();
			const stream = context.requireClient().handle(workflowId).subscribe();
			context.collector = new StreamCollector(
				stream,
				asRecord(input.collect),
				() => transport.attempts,
			);
			return { kind: "eventStreamStarted" };
		}
		case "harness.forceDisconnect": {
			const collector = context.requireCollector();
			const relay = context.requireRelay();
			await collector.waitFor(
				() => collector.events.length >= collector.minimumEventsBeforeDisconnect,
				`${collector.minimumEventsBeforeDisconnect} event(s) before the forced disconnect`,
			);
			collector.markDisconnected();
			relay.destroyConnections();
			return { kind: "disconnectInjected" };
		}
		case "harness.assertStream": {
			// Await the one collector started by the subscribe step: the
			// assertion runs over the single accumulated list spanning the
			// forced disconnect, never over a fresh subscription. Completion
			// requires events that PROVE the reconnect (delivered by a
			// transport attempt opened after the forced disconnect) plus the
			// step's own eventsInclude expectations, bounded by timeoutMs.
			const collector = context.requireCollector();
			const requiredEvents = asArray(
				asRecord(expected.ok).eventsInclude,
			).map(asRecord);
			await collector.waitFor(
				() =>
					collector.deliveredAfterDisconnect() >=
						collector.minimumEventsAfterReconnect &&
					requiredEvents.every((required) =>
						eventIncluded(collector.events, required),
					),
				`${collector.minimumEventsAfterReconnect} event(s) after the reconnect and all expected events`,
				typeof input.timeoutMs === "number" ? input.timeoutMs : undefined,
			);
			await context.stopCollector();
			const events = [...collector.events];
			return {
				kind: "eventStream",
				events,
				sequenceContiguousUnique: sequencesContiguousUnique(events),
			};
		}
		default:
			throw new InvalidArgumentError(
				`unsupported conformance operation ${operation}`,
			);
	}
}

/**
 * Deadline for the fixture workflow to finish registering its query
 * handlers: registration is workflow code racing the caller after start
 * returns, so the harness retries unknown-query outcomes within this window
 * (the same fixture-readiness convention as the committed server e2e matrix
 * in crates/aion-server/tests/query_workflow.rs and the other harnesses).
 */
const QUERY_REGISTRATION_DEADLINE_MS = 10_000;

async function queryWhenRegistered(
	context: ScenarioContext,
	input: JsonRecord,
): Promise<Json> {
	const deadline = Date.now() + QUERY_REGISTRATION_DEADLINE_MS;
	while (true) {
		try {
			return await context.requireClient().query<Json, Json>({
				workflowId: String(input.workflowId),
				runId: optionalString(input.runId),
				queryName: String(input.queryName),
				timeoutMs:
					typeof input.deadlineMs === "number" ? input.deadlineMs : 5000,
			});
		} catch (error) {
			const isUnknownQuery =
				error instanceof AionClientError &&
				error.kind === "InvalidArgument" &&
				(String(error.detail?.code ?? "").includes("unknown_query") ||
					error.message.toLowerCase().includes("unknown"));
			if (isUnknownQuery && Date.now() < deadline) {
				await delay(25);
				continue;
			}
			throw error;
		}
	}
}

async function collectStream(
	input: JsonRecord,
	context: ScenarioContext,
): Promise<JsonRecord[]> {
	const selector = asRecord(input.selector ?? input);
	const workflowId = String(selector.workflowId ?? input.workflowId ?? "");
	const stream = context.requireClient().handle(workflowId).subscribe();
	const events: JsonRecord[] = [];
	const wanted = asArray(asRecord(input.collectUntil).eventTypes).map(String);
	const timeoutMs = Number(
		asRecord(input.collectUntil).timeoutMs ??
			asRecord(input.collect).timeoutMs ??
			input.timeoutMs ??
			10000,
	);
	const iterator = stream[Symbol.asyncIterator]();
	let remainingMs = timeoutMs;
	while (remainingMs > 0) {
		const started = Date.now();
		const item = await Promise.race([
			iterator.next(),
			delay(remainingMs).then(() => null),
		]);
		if (item === null || item.done === true) {
			return events;
		}
		events.push(normalizeStreamEvent(item.value));
		if (
			wanted.length > 0 &&
			wanted.every((kind) => events.some((event) => event.type === kind))
		) {
			return events;
		}
		if (wanted.length === 0 && events.length >= 3) {
			return events;
		}
		remainingMs -= Date.now() - started;
	}
	return events;
}

function delay(milliseconds: number): Promise<void> {
	return new Promise((resolve) => {
		setTimeout(resolve, milliseconds);
	});
}

function assertMatches(
	scenario: string,
	step: string,
	actual: JsonRecord,
	expected: JsonRecord,
	context: ScenarioContext,
	errorIdentity: JsonRecord | null,
): void {
	if ("error" in expected) {
		assert.equal(
			actual.error,
			expected.error,
			`scenario=${scenario} step=${step}`,
		);
		if ("errorSameAs" in expected) {
			const reference = context.errorIdentities.get(
				String(expected.errorSameAs),
			);
			assert(
				reference !== undefined,
				`scenario=${scenario} step=${step} errorSameAs references unrecorded step ${String(expected.errorSameAs)}`,
			);
			assert(
				errorIdentity !== null,
				`scenario=${scenario} step=${step} errorSameAs requires the step to surface an error`,
			);
			assert.deepEqual(
				errorIdentity,
				reference,
				`scenario=${scenario} step=${step} errorSameAs=${String(expected.errorSameAs)}`,
			);
		}
		return;
	}
	const ok = asRecord(actual.ok);
	const expectedOk = asRecord(expected.ok);
	assert.equal(ok.kind, expectedOk.kind, `scenario=${scenario} step=${step}`);
	if (expectedOk.workflowId === "anyString")
		assertNonEmpty(ok.workflowId, scenario, step);
	if (expectedOk.runId === "anyString")
		assertNonEmpty(ok.runId, scenario, step);
	if ("payloadEquals" in expectedOk)
		assert.deepEqual(
			ok.value,
			expectedOk.payloadEquals,
			`scenario=${scenario} step=${step}`,
		);
	if ("sameHandleAs" in expectedOk) {
		// The resolver may already have replaced the `$scenario.step`
		// reference with the recorded step's ok payload; both shapes pin the
		// same handle identity, and a dangling reference is a hard failure.
		const referenceValue = expectedOk.sameHandleAs;
		let referencedOk: JsonRecord;
		if (typeof referenceValue === "string") {
			const reference = context.results.get(referenceValue.slice(1));
			assert(reference !== undefined, `scenario=${scenario} step=${step}`);
			referencedOk = asRecord(reference.ok);
		} else {
			referencedOk = asRecord(referenceValue);
			assert(
				Object.keys(referencedOk).length > 0,
				`scenario=${scenario} step=${step} sameHandleAs reference is unresolvable`,
			);
		}
		assert.equal(
			ok.workflowId,
			referencedOk.workflowId,
			`scenario=${scenario} step=${step}`,
		);
		assert.equal(
			ok.runId,
			referencedOk.runId,
			`scenario=${scenario} step=${step}`,
		);
	}
	if ("containsWorkflowRef" in expectedOk) {
		const reference = asRecord(expectedOk.containsWorkflowRef);
		assert(
			asArray(ok.workflows)
				.map(asRecord)
				.some((workflow) => workflow.workflowId === reference.workflowId),
			`scenario=${scenario} step=${step}`,
		);
	}
	if ("statusIn" in expectedOk)
		assert(
			asArray(expectedOk.statusIn).includes(ok.status),
			`scenario=${scenario} step=${step}`,
		);
	if ("eventsInclude" in expectedOk) {
		const events = asArray(ok.events).map(asRecord);
		for (const required of asArray(expectedOk.eventsInclude).map(asRecord)) {
			assert(
				eventIncluded(events, required),
				`scenario=${scenario} step=${step} required=${JSON.stringify(required)}`,
			);
		}
	}
	if (expectedOk.sequenceContiguousUnique === true)
		assert.equal(
			ok.sequenceContiguousUnique,
			true,
			`scenario=${scenario} step=${step}`,
		);
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
		if (value.startsWith("${defaults.") && value.endsWith("}"))
			return lookup(defaults, value.slice(11, -1));
		if (value.startsWith("${fixtures.") && value.endsWith("}"))
			return lookup(fixtures, value.slice(11, -1));
		if (value.startsWith("$"))
			return context.lookup(scenarioId, value.slice(1));
		return value;
	}
	if (Array.isArray(value))
		return value.map((item) =>
			resolveValue(
				item,
				defaults,
				fixtures,
				scenarioId,
				context,
				startedAt,
				now,
			),
		);
	if (value !== null && typeof value === "object") {
		const resolved: MutableJsonRecord = {};
		for (const [key, item] of Object.entries(value))
			resolved[key] = resolveValue(
				item,
				defaults,
				fixtures,
				scenarioId,
				context,
				startedAt,
				now,
			);
		return resolved;
	}
	return value ?? null;
}

/**
 * Decodes a WireEnvelope's opaque payload into its carried JSON value. AW
 * list/describe responses carry their domain values (`WorkflowSummary`,
 * `Event`) as JSON bytes inside `WireEnvelope.payload`; anything without a
 * decodable payload passes through unchanged.
 */
function wireEnvelopeJson(value: unknown): JsonRecord {
	const record = asRecord(value);
	const payload = asRecord(record.payload);
	// HTTP routes may render the payload pre-decoded as `data`; the raw wire
	// form carries JSON `bytes`.
	if (payload.data !== undefined && typeof payload.data === "object") {
		return asRecord(payload.data);
	}
	if (
		typeof payload.content_type === "string" &&
		Array.isArray(payload.bytes)
	) {
		try {
			return asRecord(
				JSON.parse(
					new TextDecoder().decode(
						Uint8Array.from(payload.bytes as number[]),
					),
				),
			);
		} catch {
			return record;
		}
	}
	return record;
}

function normalizeSummary(summary: WireEnvelope | undefined): JsonRecord {
	const record = wireEnvelopeJson(summary);
	return {
		workflowId:
			optionalString(record.workflow_id) ??
			idValue(record.workflow_id) ??
			optionalString(record.workflowId) ??
			null,
		workflowType: record.workflow_type ?? record.workflowType ?? null,
		status: statusName(record.status),
	};
}

function normalizeStreamEvent(event: WorkflowEvent): JsonRecord {
	const raw = asRecord(event.raw);
	const core = decodeCoreEvent(raw);
	if (core !== null) {
		const data = asRecord(core.data);
		return {
			type: renameEventType(String(core.type)),
			workflowId:
				optionalString(asRecord(data.envelope).workflow_id) ?? null,
			seq: event.seq,
			payload: coreUserPayload(data),
		};
	}
	const payload = asRecord(asRecord(raw.event).payload);
	return {
		type: renameEventType(
			String(raw.type ?? asRecord(raw.event).type ?? "Event"),
		),
		workflowId:
			optionalString(asRecord(event.envelope).workflowId) ??
			optionalString(asRecord(event.envelope).workflow_id) ??
			null,
		seq: event.seq,
		payload: decodePayload(payload),
	};
}

function renameEventType(type: string): string {
	return type
		.replace("SignalReceived", "WorkflowSignalled")
		.replace("WorkflowCancelled", "WorkflowCancellationRequested");
}

/**
 * Decodes the serde-encoded aion-core event nested inside a StreamedEvent
 * wire envelope (`{"type": ..., "data": {"envelope": {"seq", "workflow_id"},
 * ...}}`), which is where the real server carries event type, workflow id,
 * and user payloads.
 */
function decodeCoreEvent(raw: JsonRecord): JsonRecord | null {
	const payload = asRecord(asRecord(raw.event).payload);
	if (
		payload.content_type !== "application/json" ||
		!Array.isArray(payload.bytes)
	) {
		return null;
	}
	let parsed: unknown;
	try {
		parsed = JSON.parse(
			new TextDecoder().decode(Uint8Array.from(payload.bytes as number[])),
		);
	} catch {
		return null;
	}
	const record = asRecord(parsed);
	return typeof record.type === "string" && "data" in record ? record : null;
}

/**
 * Extracts the user-visible payload from a core event's variant data: the
 * opaque `Payload` field (`payload` for signals, `input` for starts,
 * `result` for completions), decoded from its JSON bytes.
 */
function coreUserPayload(data: JsonRecord): Json {
	for (const key of ["payload", "input", "result"]) {
		const candidate = asRecord(data[key]);
		// HTTP routes may render the user payload pre-decoded as `data`.
		if (typeof candidate.content_type === "string" && "data" in candidate) {
			return candidate.data ?? null;
		}
		if (
			typeof candidate.content_type === "string" &&
			Array.isArray(candidate.bytes)
		) {
			try {
				return JSON.parse(
					new TextDecoder().decode(
						Uint8Array.from(candidate.bytes as number[]),
					),
				) as Json;
			} catch {
				return null;
			}
		}
	}
	return null;
}

function normalizeEnvelopeEvent(envelope: WireEnvelope): JsonRecord {
	const record = wireEnvelopeJson(envelope);
	// Decoded aion-core event shape: {"type": ..., "data": {"envelope":
	// {"seq", "workflow_id"}, <payload|input|result>}}.
	if (typeof record.type === "string" && "data" in record) {
		const data = asRecord(record.data);
		const eventEnvelope = asRecord(data.envelope);
		return {
			type: renameEventType(String(record.type)),
			workflowId: optionalString(eventEnvelope.workflow_id) ?? null,
			seq: Number(eventEnvelope.seq ?? 0),
			payload: coreUserPayload(data),
		};
	}
	return {
		type: renameEventType(String(record.type ?? "Event")),
		workflowId:
			optionalString(asRecord(record.envelope).workflowId) ??
			idValue(asRecord(record.envelope).workflow_id) ??
			null,
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
	const payload = value as unknown as Payload;
	try {
		return fromPayload<Json>(payload);
	} catch {
		return value ?? null;
	}
}

function eventIncluded(
	events: readonly JsonRecord[],
	required: JsonRecord,
): boolean {
	return events.some((event) =>
		Object.entries(required).every(([key, value]) => {
			if (key === "payloadContains") {
				const payload = asRecord(event.payload);
				return Object.entries(asRecord(value)).every(
					([payloadKey, payloadValue]) => payload[payloadKey] === payloadValue,
				);
			}
			return event[key] === value;
		}),
	);
}

function sequencesContiguousUnique(events: readonly JsonRecord[]): boolean {
	const sequences = events
		.map((event) => event.seq)
		.filter((seq): seq is number => typeof seq === "number")
		.sort((left, right) => left - right);
	return (
		sequences.length > 0 &&
		sequences.slice(1).every((seq, index) => seq === sequences[index] + 1)
	);
}

function payloadJson(value: Json): Json {
	return asRecord(value).json ?? null;
}

function lookup(root: JsonRecord, path: string): Json {
	return path
		.split(".")
		.reduce<Json>((current, part) => asRecord(current)[part] ?? null, root);
}

function idValue(value: Json): string | undefined {
	const uuid = asRecord(value).uuid;
	return typeof uuid === "string" ? uuid : undefined;
}

function optionalString(value: Json | undefined): string | undefined {
	return typeof value === "string" ? value : undefined;
}

function statusName(value: Json): Json {
	if (typeof value === "number")
		return (
			["UNKNOWN", "RUNNING", "COMPLETED", "FAILED", "CANCELLED", "TIMED_OUT"][
				value
			] ?? String(value)
		);
	if (typeof value === "string") {
		// serde renders aion-core WorkflowStatus in PascalCase; the scenario
		// document uses the language-neutral SCREAMING_SNAKE spelling.
		const names: Record<string, string> = {
			Running: "RUNNING",
			Completed: "COMPLETED",
			Failed: "FAILED",
			Cancelled: "CANCELLED",
			TimedOut: "TIMED_OUT",
			ContinuedAsNew: "CONTINUED_AS_NEW",
		};
		return names[value] ?? value;
	}
	return value;
}

function asRecord(value: unknown): JsonRecord {
	return value !== null && typeof value === "object" && !Array.isArray(value)
		? (value as JsonRecord)
		: {};
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
	relay?: TcpRelay;
	streamTransport?: StoppableSubscribeTransport;
	collector?: StreamCollector;
	readonly results = new Map<string, JsonRecord>();
	readonly errorIdentities = new Map<string, JsonRecord>();

	requireClient(): Client {
		if (this.client === undefined)
			throw new Error("scenario has not connected yet");
		return this.client;
	}

	requireRelay(): TcpRelay {
		if (this.relay === undefined)
			throw new Error(
				"forced stream disconnects require the harness TCP relay, which " +
					"only fronts plaintext http endpoints; AION_SERVER_URL is not http",
			);
		return this.relay;
	}

	requireCollector(): StreamCollector {
		if (this.collector === undefined)
			throw new Error(
				"no stream collector is running; the subscribe step with a " +
					"`collect` input must run before harness stream operations",
			);
		return this.collector;
	}

	requireStreamTransport(): StoppableSubscribeTransport {
		if (this.streamTransport === undefined)
			throw new Error("scenario has not connected yet");
		return this.streamTransport;
	}

	/**
	 * Stops the background collector deterministically: severing the relayed
	 * stream sockets forces the transport into its reconnect path, where the
	 * stopped wrapper raises the terminal harness-stop error the collector
	 * treats as a clean end.
	 */
	async stopCollector(): Promise<void> {
		if (this.collector === undefined) return;
		this.streamTransport?.stop();
		this.relay?.destroyConnections();
		await this.collector.finished;
	}

	async teardown(): Promise<void> {
		await this.stopCollector();
		this.streamTransport?.stop();
		if (this.relay !== undefined) await this.relay.close();
	}

	record(scenario: string, step: string, value: JsonRecord): void {
		this.results.set(`${scenario}.${step}`, value);
	}

	recordErrorIdentity(
		scenario: string,
		step: string,
		identity: JsonRecord,
	): void {
		this.errorIdentities.set(`${scenario}.${step}`, identity);
	}

	lookup(currentScenario: string, path: string): Json {
		const parts = path.split(".");
		const scenario =
			parts.length > 1 && this.results.has(`${parts[0]}.${parts[1]}`)
				? String(parts.shift())
				: currentScenario;
		const step = String(parts.shift());
		let value: Json = this.results.get(`${scenario}.${step}`) ?? null;
		if ("ok" in asRecord(value)) value = asRecord(value).ok ?? null;
		for (const part of parts) value = asRecord(value)[part] ?? null;
		return value;
	}
}

const HARNESS_STOP_MESSAGE = "conformance harness stopped the stream collector";

function isHarnessStop(error: unknown): boolean {
	return error instanceof CancelledError && error.message === HARNESS_STOP_MESSAGE;
}

/**
 * Transparent local TCP relay in front of the aion server. The SDK's
 * built-in WebSocket transport connects through it, so destroying the
 * currently piped sockets injects a real transient disconnect while the
 * listener keeps accepting the reconnect.
 */
class TcpRelay {
	private constructor(
		private readonly server: net.Server,
		readonly port: number,
		private readonly sockets: Set<net.Socket>,
	) {}

	static async start(targetHost: string, targetPort: number): Promise<TcpRelay> {
		const sockets = new Set<net.Socket>();
		const server = net.createServer((downstream) => {
			const upstream = net.connect(targetPort, targetHost);
			sockets.add(downstream);
			sockets.add(upstream);
			const sever = () => {
				sockets.delete(downstream);
				sockets.delete(upstream);
				downstream.destroy();
				upstream.destroy();
			};
			downstream.on("error", sever);
			upstream.on("error", sever);
			downstream.on("close", sever);
			upstream.on("close", sever);
			downstream.pipe(upstream);
			upstream.pipe(downstream);
		});
		await new Promise<void>((resolve, reject) => {
			server.once("error", reject);
			server.listen(0, "127.0.0.1", resolve);
		});
		const address = server.address();
		if (address === null || typeof address === "string")
			throw new Error("harness TCP relay did not bind a TCP port");
		return new TcpRelay(server, address.port, sockets);
	}

	/** Destroys every piped socket; the listener stays up for reconnects. */
	destroyConnections(): void {
		for (const socket of [...this.sockets]) socket.destroy();
		this.sockets.clear();
	}

	async close(): Promise<void> {
		this.destroyConnections();
		await new Promise<void>((resolve, reject) => {
			this.server.close((error) =>
				error === undefined ? resolve() : reject(error),
			);
		});
	}
}

/**
 * Wraps the built-in transport for the harness. All real work (upgrade
 * headers, subscription frame, resume cursor) flows through the wrapped
 * built-in transport, with two harness behaviours on top:
 *
 * - The scenarios assert on events from the workflow's beginning
 *   (`eventsInclude` lists `WorkflowStarted`), so the initial attach maps to
 *   the wire's documented full-history replay, `resume_from_seq = 1`. An
 *   absent cursor would be a live tail that can never deliver events emitted
 *   before the subscription attached. Reconnect cursors (lastDelivered + 1)
 *   pass through untouched.
 * - Once stopped, the next (re)subscribe raises a terminal error so the
 *   resume loop ends deterministically instead of reconnecting forever.
 */
class StoppableSubscribeTransport implements SubscribeTransport {
	/** Count of transport attempts opened; serial, by the resume loop. */
	attempts = 0;
	private stopped = false;

	constructor(private readonly inner: SubscribeTransport) {}

	stop(): void {
		this.stopped = true;
	}

	async *subscribe(request: SubscribeRequest): AsyncIterable<unknown> {
		if (this.stopped) throw new CancelledError(HARNESS_STOP_MESSAGE);
		this.attempts += 1;
		const fromStart: SubscribeRequest = {
			...request,
			resumeFrom: request.resumeFrom ?? 1,
		};
		for await (const frame of this.inner.subscribe(fromStart)) {
			if (this.stopped) throw new CancelledError(HARNESS_STOP_MESSAGE);
			yield frame;
		}
	}
}

/**
 * The single background collector for the disconnect-resume choreography.
 * It consumes the one stream opened by the subscribe step and accumulates
 * every delivered event across the forced disconnect, so assertions run
 * over one contiguous list rather than a fresh subscription.
 */
class StreamCollector {
	readonly events: JsonRecord[] = [];
	readonly minimumEventsBeforeDisconnect: number;
	readonly minimumEventsAfterReconnect: number;
	readonly timeoutMs: number;
	readonly finished: Promise<void>;
	eventsAtDisconnect: number | null = null;
	private readonly currentAttempt: () => number;
	private readonly attempts: number[] = [];
	private attemptAtDisconnect: number | null = null;
	private failure: unknown = null;
	private ended = false;
	private waiters: Array<() => void> = [];

	constructor(
		stream: AsyncIterable<WorkflowEvent>,
		collect: JsonRecord,
		currentAttempt: () => number,
	) {
		this.minimumEventsBeforeDisconnect = collectCount(
			collect,
			"minimumEventsBeforeDisconnect",
		);
		this.minimumEventsAfterReconnect = collectCount(
			collect,
			"minimumEventsAfterReconnect",
		);
		const timeoutMs = collect.timeoutMs;
		if (typeof timeoutMs !== "number" || timeoutMs <= 0)
			throw new Error(
				"subscribe collect input requires a positive timeoutMs bounding the harness waits",
			);
		this.timeoutMs = timeoutMs;
		this.currentAttempt = currentAttempt;
		this.finished = this.collect(stream);
	}

	markDisconnected(): void {
		this.eventsAtDisconnect = this.events.length;
		this.attemptAtDisconnect = this.currentAttempt();
	}

	/**
	 * Events that PROVE the reconnect: delivered by a transport attempt
	 * opened after the forced disconnect. In-flight events buffered from the
	 * severed attempt never count, so a premature pass is impossible.
	 */
	deliveredAfterDisconnect(): number {
		const boundary = this.attemptAtDisconnect;
		if (boundary === null) return 0;
		return this.attempts.filter((attempt) => attempt > boundary).length;
	}

	async waitFor(
		condition: () => boolean,
		what: string,
		timeoutMs?: number,
	): Promise<void> {
		const deadline = Date.now() + (timeoutMs ?? this.timeoutMs);
		while (!condition()) {
			if (this.failure !== null) throw this.failure;
			if (this.ended)
				throw new Error(
					`stream ended after ${this.events.length} event(s), before ${what}`,
				);
			const remaining = deadline - Date.now();
			if (remaining <= 0)
				throw new Error(
					`timed out waiting for ${what}; ${this.events.length} event(s) ` +
						`delivered, ${String(this.eventsAtDisconnect)} at disconnect`,
				);
			await this.nextChange(remaining);
		}
	}

	private async collect(stream: AsyncIterable<WorkflowEvent>): Promise<void> {
		try {
			for await (const event of stream) {
				this.events.push(normalizeStreamEvent(event));
				this.attempts.push(this.currentAttempt());
				this.wake();
			}
		} catch (error) {
			if (!isHarnessStop(error)) this.failure = error;
		} finally {
			this.ended = true;
			this.wake();
		}
	}

	private wake(): void {
		const waiters = this.waiters;
		this.waiters = [];
		for (const waiter of waiters) waiter();
	}

	private nextChange(maxWaitMs: number): Promise<void> {
		return new Promise<void>((resolve) => {
			const timer = setTimeout(resolve, maxWaitMs);
			this.waiters.push(() => {
				clearTimeout(timer);
				resolve();
			});
		});
	}
}

function collectCount(collect: JsonRecord, key: string): number {
	const value = collect[key] ?? 0;
	if (typeof value !== "number" || !Number.isInteger(value) || value < 0)
		throw new Error(`subscribe collect input ${key} must be a non-negative integer`);
	return value;
}
