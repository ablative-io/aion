import { status } from "@grpc/grpc-js";
import { describe, expect, it } from "vitest";
import { defineActivity } from "./activity.js";
import { Worker } from "./worker.js";
import type {
	ActivityFailure,
	ActivityTask,
	Payload,
	WorkerConfig,
	WorkerSession,
	WorkerSessionEvent,
} from "./session.js";

type SessionScriptEvent =
	| WorkerSessionEvent
	| { readonly kind: "throw"; readonly error: Error };

/**
 * In-memory session for Worker.run tests: tasks and stream failures are
 * pushed while the run is live, registrations / reports / heartbeats are
 * recorded, and close() ends the receive stream the way a real transport
 * close does.
 */
class FakeWorkerSession implements WorkerSession {
	public readonly completed: Payload[] = [];
	public readonly failures: ActivityFailure[] = [];
	public readonly registrations: string[][] = [];
	public readonly heartbeats: Array<{
		readonly workflowId: string;
		readonly activityId: string;
	}> = [];
	public heartbeatError?: Error;
	public closed = false;
	private readonly events: SessionScriptEvent[] = [];
	private resolver?: () => void;

	public handshake(): Promise<void> {
		return Promise.resolve();
	}

	public async register(activityTypes: readonly string[]): Promise<void> {
		// Fidelity with the real transport: registering on a closed session
		// is a write into an ended stream and must fail, never resolve.
		if (this.closed) {
			throw new Error("write after end");
		}
		this.registrations.push([...activityTypes]);
	}

	public async *receiveTasks(): AsyncIterable<WorkerSessionEvent> {
		for (;;) {
			const event = this.events.shift();
			if (event !== undefined) {
				if (event.kind === "throw") {
					throw event.error;
				}
				yield event;
				continue;
			}
			if (this.closed) {
				yield { kind: "closed" };
				return;
			}
			await new Promise<void>((resolve) => {
				this.resolver = resolve;
			});
		}
	}

	public async reportResult(
		_workflowId: string,
		_activityId: string,
		result: Payload,
	): Promise<void> {
		this.completed.push(result);
	}

	public async reportFailure(
		_workflowId: string,
		_activityId: string,
		failure: ActivityFailure,
	): Promise<void> {
		this.failures.push(failure);
	}

	public async sendHeartbeat(
		workflowId: string,
		activityId: string,
	): Promise<void> {
		if (this.heartbeatError !== undefined) {
			throw this.heartbeatError;
		}
		this.heartbeats.push({ workflowId, activityId });
	}

	public async close(): Promise<void> {
		this.closed = true;
		this.wake();
	}

	public push(event: WorkerSessionEvent): void {
		this.events.push(event);
		this.wake();
	}

	public failStream(error: Error): void {
		this.events.push({ kind: "throw", error });
		this.wake();
	}

	private wake(): void {
		const resolver = this.resolver;
		this.resolver = undefined;
		resolver?.();
	}
}

describe("Worker", () => {
	it("drains in-flight activities before run returns on shutdown", async () => {
		const session = new FakeWorkerSession();
		let handlerStarted: (() => void) | undefined;
		const started = new Promise<void>((resolve) => {
			handlerStarted = resolve;
		});
		let finishSlow: (() => void) | undefined;
		let handlerCompleted = false;
		let cancellationObserved = false;
		const worker = new Worker(
			config(),
			[
				defineActivity("slow", async (_input, ctx) => {
					handlerStarted?.();
					await new Promise<void>((resolve) => {
						finishSlow = resolve;
					});
					cancellationObserved = ctx.isCancelled();
					handlerCompleted = true;
					return { done: true };
				}),
			],
			{ sessionFactory: async () => session },
		);
		const controller = new AbortController();
		let runSettled = false;
		const run = worker.run({ signal: controller.signal }).then(() => {
			runSettled = true;
		});

		session.push({ kind: "task", task: task("slow", {}) });
		await started;
		controller.abort();
		await macrotaskTurns(5);

		// The abort has propagated (session closed, cancellation signalled)
		// but the in-flight handler has not finished: run must still be
		// pending and nothing may have been reported yet.
		expect(runSettled).toBe(false);
		expect(handlerCompleted).toBe(false);
		expect(session.completed).toHaveLength(0);

		expect(finishSlow).toBeDefined();
		finishSlow?.();
		await run;

		expect(runSettled).toBe(true);
		expect(handlerCompleted).toBe(true);
		expect(cancellationObserved).toBe(true);
		expect(session.completed).toHaveLength(1);
	});

	it("serves a fake-session task end to end", async () => {
		const session = new FakeWorkerSession();
		const worker = new Worker(
			config(),
			[
				defineActivity<{ readonly value: number }, { readonly value: number }>(
					"increment",
					async (input) => ({ value: input.value + 1 }),
				),
			],
			{ sessionFactory: async () => session },
		);
		const controller = new AbortController();
		const run = worker.run({ signal: controller.signal });

		session.push({ kind: "task", task: task("increment", { value: 6 }) });
		await waitFor(() => session.completed.length === 1);
		controller.abort();
		await run;

		expect(session.registrations).toEqual([["increment"]]);
		expect(session.failures).toEqual([]);
		expect(decode(session.completed[0] as Payload)).toEqual({ value: 7 });
	});

	it("reconnects after a transient stream drop, re-registers, and keeps serving", async () => {
		const first = new FakeWorkerSession();
		const second = new FakeWorkerSession();
		const sessions = [first, second];
		let factoryCalls = 0;
		const worker = new Worker(
			config(),
			[
				defineActivity<{ readonly value: number }, { readonly value: number }>(
					"increment",
					async (input) => ({ value: input.value + 1 }),
				),
			],
			{
				sessionFactory: async () => {
					const session = sessions[factoryCalls];
					factoryCalls += 1;
					if (session === undefined) {
						throw new Error("session factory exhausted");
					}
					return session;
				},
			},
		);
		const controller = new AbortController();
		const run = worker.run({ signal: controller.signal });

		first.push({ kind: "task", task: task("increment", { value: 1 }, "t1") });
		await waitFor(() => first.completed.length === 1);
		first.failStream(
			serviceError(status.UNAVAILABLE, "14 UNAVAILABLE: stream reset"),
		);
		await waitFor(() => second.registrations.length === 1);

		second.push({ kind: "task", task: task("increment", { value: 5 }, "t2") });
		// The first session's result was never acknowledged, so it is
		// re-reported on the new session before the new task's result lands.
		await waitFor(() => second.completed.length === 2);
		controller.abort();
		await run;

		expect(factoryCalls).toBe(2);
		expect(first.closed).toBe(true);
		expect(first.registrations).toEqual([["increment"]]);
		expect(second.registrations).toEqual([["increment"]]);
		expect(decode(second.completed[0] as Payload)).toEqual({ value: 2 });
		expect(decode(second.completed[1] as Payload)).toEqual({ value: 6 });
		expect(second.failures).toEqual([]);
	});

	it("fails fast on a server denial after exactly one connection attempt", async () => {
		const session = new FakeWorkerSession();
		const denial = serviceError(
			status.PERMISSION_DENIED,
			"7 PERMISSION_DENIED: namespace 'payments' is not granted",
		);
		let factoryCalls = 0;
		const worker = new Worker(
			config(),
			[defineActivity("increment", async () => ({}))],
			{
				sessionFactory: async () => {
					factoryCalls += 1;
					return session;
				},
			},
		);
		const run = worker.run();

		session.failStream(denial);

		await expect(run).rejects.toBe(denial);
		expect(factoryCalls).toBe(1);
		expect(session.closed).toBe(true);
	});

	it("routes an activity heartbeat to the new session after a reconnect", async () => {
		const first = new FakeWorkerSession();
		const second = new FakeWorkerSession();
		const sessions = [first, second];
		let factoryCalls = 0;
		const worker = new Worker(
			config(),
			[
				defineActivity("beat", async (_input, ctx) => {
					await ctx.heartbeat();
					return { ok: true };
				}),
			],
			{
				sessionFactory: async () => {
					const session = sessions[factoryCalls];
					factoryCalls += 1;
					if (session === undefined) {
						throw new Error("session factory exhausted");
					}
					return session;
				},
			},
		);
		const controller = new AbortController();
		const run = worker.run({ signal: controller.signal });

		first.failStream(
			serviceError(status.UNAVAILABLE, "14 UNAVAILABLE: stream reset"),
		);
		await waitFor(() => second.registrations.length === 1);
		second.push({ kind: "task", task: task("beat", {}, "h2") });
		await waitFor(() => second.heartbeats.length === 1);
		controller.abort();
		await run;

		expect(first.heartbeats).toEqual([]);
		expect(second.heartbeats).toEqual([
			{ workflowId: "workflow", activityId: "h2" },
		]);
		expect(second.completed).toHaveLength(1);
	});

	it("logs and swallows a heartbeat into the dead session during the reconnect window", async () => {
		const first = new FakeWorkerSession();
		const second = new FakeWorkerSession();
		const sessions = [first, second];
		let factoryCalls = 0;
		let handlerStarted: (() => void) | undefined;
		const started = new Promise<void>((resolve) => {
			handlerStarted = resolve;
		});
		let releaseHandler: (() => void) | undefined;
		const gate = new Promise<void>((resolve) => {
			releaseHandler = resolve;
		});
		const warnings: Array<{
			readonly message: string;
			readonly fields?: Record<string, unknown>;
		}> = [];
		const worker = new Worker(
			config(),
			[
				defineActivity("patient", async (_input, ctx) => {
					handlerStarted?.();
					await gate;
					// The stream has already failed by now; this heartbeat hits
					// the dead session and must not throw into the handler.
					await ctx.heartbeat({ stage: "late" });
					return { ok: true };
				}),
			],
			{
				sessionFactory: async () => {
					const session = sessions[factoryCalls];
					factoryCalls += 1;
					if (session === undefined) {
						throw new Error("session factory exhausted");
					}
					return session;
				},
				logger: {
					info: () => undefined,
					warn: (message, fields) => {
						warnings.push({ message, fields });
					},
					error: () => undefined,
				},
			},
		);
		const controller = new AbortController();
		const run = worker.run({ signal: controller.signal });

		first.push({ kind: "task", task: task("patient", {}, "p1") });
		await started;
		first.heartbeatError = new Error("stream is dead");
		first.failStream(
			serviceError(status.UNAVAILABLE, "14 UNAVAILABLE: stream reset"),
		);
		releaseHandler?.();
		// The activity result is re-reported on the replacement session once
		// the reconnect completes, proving the handler survived the failed
		// heartbeat and finished normally.
		await waitFor(() => second.completed.length === 1);
		controller.abort();
		await run;

		expect(first.heartbeats).toEqual([]);
		expect(second.failures).toEqual([]);
		const heartbeatWarnings = warnings.filter(
			(warning) =>
				warning.message ===
				"activity heartbeat failed; session may be reconnecting",
		);
		expect(heartbeatWarnings).toHaveLength(1);
		expect(heartbeatWarnings[0]?.fields).toEqual({
			workflowId: "workflow",
			activityId: "p1",
			message: "stream is dead",
		});
	});

	it("rejects run when the reconnect settings are invalid before connecting", async () => {
		const session = new FakeWorkerSession();
		let factoryCalls = 0;
		const worker = new Worker(
			{
				endpoint: "127.0.0.1:50051",
				taskQueue: "queue",
				identity: "identity",
				maxConcurrency: 1,
				reconnect: { initialDelayMs: 1, maxDelayMs: 2, maxAttempts: 0 },
			},
			[defineActivity("increment", async () => ({}))],
			{
				sessionFactory: async () => {
					factoryCalls += 1;
					return session;
				},
			},
		);

		await expect(worker.run()).rejects.toThrow(
			"worker reconnect maxAttempts must be a positive integer",
		);
		expect(factoryCalls).toBe(0);
	});

	it("returns without serving when the signal is already aborted before run", async () => {
		const session = new FakeWorkerSession();
		let factoryCalls = 0;
		const worker = new Worker(
			config(),
			[defineActivity("increment", async () => ({}))],
			{
				sessionFactory: async () => {
					factoryCalls += 1;
					return session;
				},
			},
		);
		const controller = new AbortController();
		controller.abort();

		await worker.run({ signal: controller.signal });

		expect(factoryCalls).toBe(0);
		expect(session.registrations).toEqual([]);
		expect(session.completed).toEqual([]);
	});

	it("returns gracefully when an abort closes the session before the register write", async () => {
		const controller = new AbortController();
		// The abort lands during the handshake, after run()'s pre-abort check:
		// the abort handler closes the session, so the register write fails
		// with write-after-end (the fake's register enforces that fidelity).
		// A graceful shutdown must not become a rejected run.
		const session = new (class extends FakeWorkerSession {
			public override async handshake(): Promise<void> {
				controller.abort();
			}
		})();
		const worker = new Worker(
			config(),
			[defineActivity("increment", async () => ({}))],
			{ sessionFactory: async () => session },
		);

		await worker.run({ signal: controller.signal });

		expect(session.closed).toBe(true);
		expect(session.registrations).toEqual([]);
		expect(session.completed).toEqual([]);
	});
});

async function macrotaskTurns(turns: number): Promise<void> {
	for (let turn = 0; turn < turns; turn += 1) {
		await new Promise<void>((resolve) => {
			setTimeout(resolve, 0);
		});
	}
}

async function waitFor(
	condition: () => boolean,
	timeoutMs = 1_000,
): Promise<void> {
	const deadline = Date.now() + timeoutMs;
	while (!condition()) {
		if (Date.now() > deadline) {
			throw new Error("waitFor condition not met within timeout");
		}
		await macrotaskTurns(1);
	}
}

function serviceError(code: number, message: string): Error {
	return Object.assign(new Error(message), {
		code,
		details: message,
		metadata: {},
	});
}

function task(
	activityType: string,
	input: unknown,
	activityId = "activity",
): ActivityTask {
	return {
		workflowId: "workflow",
		activityId,
		activityType,
		input: {
			contentType: "application/json",
			bytes: new TextEncoder().encode(JSON.stringify(input)),
		},
		attempt: 1,
	};
}

function decode(payload: Payload): unknown {
	return JSON.parse(new TextDecoder().decode(payload.bytes)) as unknown;
}

function config(): WorkerConfig {
	return {
		endpoint: "127.0.0.1:50051",
		taskQueue: "queue",
		identity: "identity",
		maxConcurrency: 1,
		reconnect: {
			initialDelayMs: 1,
			maxDelayMs: 2,
			maxAttempts: 2,
		},
	};
}
