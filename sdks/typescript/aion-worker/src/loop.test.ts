import { status } from "@grpc/grpc-js";
import { describe, expect, it } from "vitest";
import {
	type ActivityDispatcher,
	type DispatchOutcome,
	runWorkerLoop,
} from "./loop.js";
import { ReconnectExhaustedError, UnackedResultTracker } from "./reconnect.js";
import type {
	ActivityFailure,
	ActivityTask,
	Payload,
	WorkerConfig,
	WorkerSession,
	WorkerSessionEvent,
} from "./session.js";

const payload: Payload = {
	contentType: "application/json",
	bytes: new Uint8Array([123, 125]),
};

class ArraySession implements WorkerSession {
	public readonly reports: string[] = [];

	public constructor(private readonly events: readonly WorkerSessionEvent[]) {}

	public handshake(): Promise<void> {
		return Promise.resolve();
	}

	public register(): Promise<void> {
		return Promise.resolve();
	}

	public async *receiveTasks(): AsyncIterable<WorkerSessionEvent> {
		for (const event of this.events) {
			yield event;
		}
	}

	public async reportResult(
		_workflowId: string,
		activityId: string,
	): Promise<void> {
		this.reports.push(activityId);
	}

	public async reportFailure(
		_workflowId: string,
		activityId: string,
		_failure: ActivityFailure,
	): Promise<void> {
		this.reports.push(activityId);
	}

	public sendHeartbeat(): Promise<void> {
		return Promise.resolve();
	}

	public close(): Promise<void> {
		return Promise.resolve();
	}
}

class SlowDispatcher implements ActivityDispatcher {
	public current = 0;
	public peak = 0;

	public activityTypes(): readonly string[] {
		return ["slow"];
	}

	public async dispatch(): Promise<DispatchOutcome> {
		this.current += 1;
		this.peak = Math.max(this.peak, this.current);
		try {
			await new Promise<void>((resolve) => {
				setTimeout(resolve, 5);
			});
			return { kind: "completed", output: payload };
		} finally {
			this.current -= 1;
		}
	}
}

describe("runWorkerLoop", () => {
	it("bounds concurrent dispatch at maxConcurrency", async () => {
		const tasks = Array.from({ length: 5 }, (_, index) =>
			task(String(index + 1)),
		);
		const session = new ArraySession([
			...tasks.map((item) => ({ kind: "task" as const, task: item })),
			{ kind: "closed" },
		]);
		const dispatcher = new SlowDispatcher();

		await runWorkerLoop({
			config: config(2),
			session,
			dispatcher,
		});

		expect(dispatcher.peak).toBe(2);
		expect(session.reports).toHaveLength(5);
	});

	it("logs and reports unclassified dispatcher errors as retryable failures", async () => {
		const session = new ArraySession([
			{ kind: "task", task: task("1") },
			{ kind: "closed" },
		]);
		const warnings: string[] = [];
		const dispatcher: ActivityDispatcher = {
			activityTypes: () => ["slow"],
			dispatch: async () => {
				throw new Error("boom");
			},
		};

		await runWorkerLoop({
			config: config(1),
			session,
				dispatcher,
				logger: {
					info: () => undefined,
					warn: (message, fields) => {
						warnings.push(`${message}:${String(fields?.retryable)}`);
					},
					error: () => undefined,
				},
			});

		expect(session.reports).toEqual(["1"]);
		expect(warnings).toEqual([
			"worker dispatcher threw unclassified error:true",
		]);
	});

	it("surfaces PERMISSION_DENIED from the receive stream after exactly one connection", async () => {
		const denial = serviceError(
			status.PERMISSION_DENIED,
			"7 PERMISSION_DENIED: namespace 'payments' is not granted",
		);
		const session = new ScriptedSession([{ kind: "throw", error: denial }]);
		const errors: Array<Record<string, unknown> | undefined> = [];
		let factoryCalls = 0;
		const sleeps: number[] = [];

		const failure = await runWorkerLoop({
			config: reconnectingConfig(),
			session,
			dispatcher: new SlowDispatcher(),
			sessionFactory: async () => {
				factoryCalls += 1;
				return new ScriptedSession([{ kind: "closed" }]);
			},
			sleep: async (delayMs) => {
				sleeps.push(delayMs);
			},
			logger: {
				info: () => undefined,
				warn: () => undefined,
				error: (_message, fields) => {
					errors.push(fields);
				},
			},
		}).then(
			() => {
				throw new Error("expected runWorkerLoop to reject");
			},
			(error: unknown) => error,
		);

		expect(failure).toBe(denial);
		expect((failure as { readonly details?: string }).details).toBe(
			"7 PERMISSION_DENIED: namespace 'payments' is not granted",
		);
		expect(factoryCalls).toBe(0);
		expect(sleeps).toEqual([]);
		expect(session.closed).toBe(true);
		expect(errors).toEqual([
			{
				code: status.PERMISSION_DENIED,
				message: denial.message,
			},
		]);
	});

	it("surfaces UNAUTHENTICATED from the receive stream after exactly one connection", async () => {
		const denial = serviceError(
			status.UNAUTHENTICATED,
			"16 UNAUTHENTICATED: worker credentials were rejected",
		);
		const session = new ScriptedSession([{ kind: "throw", error: denial }]);
		let factoryCalls = 0;

		await expect(
			runWorkerLoop({
				config: reconnectingConfig(),
				session,
				dispatcher: new SlowDispatcher(),
				sessionFactory: async () => {
					factoryCalls += 1;
					return new ScriptedSession([{ kind: "closed" }]);
				},
				sleep: () => Promise.resolve(),
			}),
		).rejects.toBe(denial);

		expect(factoryCalls).toBe(0);
		expect(session.closed).toBe(true);
	});

	it("reconnects within the bounded budget after an UNAVAILABLE stream error", async () => {
		const unavailable = serviceError(
			status.UNAVAILABLE,
			"14 UNAVAILABLE: stream reset by transport",
		);
		const finalDenial = serviceError(
			status.PERMISSION_DENIED,
			"7 PERMISSION_DENIED: namespace 'payments' was revoked",
		);
		const first = new ScriptedSession([{ kind: "throw", error: unavailable }]);
		const second = new ScriptedSession([
			{ kind: "task", task: task("after-reconnect") },
			{ kind: "throw", error: finalDenial },
		]);
		const sleeps: number[] = [];
		let factoryCalls = 0;

		await expect(
			runWorkerLoop({
				config: reconnectingConfig(),
				session: first,
				dispatcher: new SlowDispatcher(),
				sessionFactory: async () => {
					factoryCalls += 1;
					if (factoryCalls === 1) {
						throw serviceError(
							status.UNAVAILABLE,
							"14 UNAVAILABLE: engine still unreachable",
						);
					}
					return second;
				},
				sleep: async (delayMs) => {
					sleeps.push(delayMs);
				},
			}),
		).rejects.toBe(finalDenial);

		// The UNAVAILABLE stream error fed the bounded reconnect path: the
		// drop itself backed off once (inter-cycle delay), the first
		// establishment attempt failed and slept once more, and the second
		// attempt produced the working session whose task was served before
		// its own stream denial ended the loop fail-fast.
		expect(factoryCalls).toBe(2);
		expect(sleeps).toEqual([1, 1]);
		expect(first.closed).toBe(true);
		expect(second.reports).toEqual(["after-reconnect"]);
		expect(second.closed).toBe(true);
	});

	it("reconnects on clean closes but bounds them with the cumulative drop budget", async () => {
		const first = new ScriptedSession([{ kind: "closed" }]);
		// The second session's iterator completes without yielding `closed`,
		// covering the `done: true` clean-completion path as well.
		const second = new ScriptedSession([{ kind: "task", task: task("1") }]);
		const third = new ScriptedSession([{ kind: "closed" }]);
		const replacements = [second, third];
		const sleeps: number[] = [];
		let factoryCalls = 0;

		const failure = await runWorkerLoop({
			config: reconnectingConfig(3),
			session: first,
			dispatcher: new SlowDispatcher(),
			sessionFactory: async () => {
				const session = replacements[factoryCalls];
				factoryCalls += 1;
				if (session === undefined) {
					throw new Error("session factory exhausted");
				}
				return session;
			},
			sleep: async (delayMs) => {
				sleeps.push(delayMs);
			},
		}).then(
			() => {
				throw new Error("expected runWorkerLoop to reject");
			},
			(error: unknown) => error,
		);

		// Each clean close re-entered the reconnect cycle (the task on the
		// second session was still served), consumed one unit of the shared
		// drop budget, and backed off on the config's own schedule; the third
		// clean close exhausted the budget and surfaced a classified error.
		expect(failure).toBeInstanceOf(ReconnectExhaustedError);
		expect((failure as Error).message).toContain(
			"worker session drop budget exhausted after 3 dropped sessions",
		);
		expect(((failure as Error).cause as Error).message).toBe(
			"worker receive stream closed cleanly while a session factory was configured",
		);
		expect(factoryCalls).toBe(2);
		expect(sleeps).toEqual([1, 2]);
		expect(second.reports).toEqual(["1"]);
		// Replaced sessions are closed before their replacements are dialled.
		expect(first.closed).toBe(true);
		expect(second.closed).toBe(true);
		expect(third.closed).toBe(true);
	});

	it("exhausts the cumulative drop budget across cycles and surfaces the last underlying error", async () => {
		const firstDrop = serviceError(
			status.UNAVAILABLE,
			"14 UNAVAILABLE: stream reset by transport",
		);
		// Not grpc-js ServiceError shaped (connect-es style array details):
		// the shape-gate classifies it retryable, so it must consume the
		// bounded budget and be preserved as the exhaustion cause.
		const lastDrop = Object.assign(
			new Error("connection reset by load balancer"),
			{ code: status.PERMISSION_DENIED, details: ["denied"] },
		);
		const first = new ScriptedSession([{ kind: "throw", error: firstDrop }]);
		const second = new ScriptedSession([{ kind: "throw", error: lastDrop }]);
		const sleeps: number[] = [];
		let factoryCalls = 0;

		const failure = await runWorkerLoop({
			config: reconnectingConfig(2),
			session: first,
			dispatcher: new SlowDispatcher(),
			sessionFactory: async () => {
				factoryCalls += 1;
				return second;
			},
			sleep: async (delayMs) => {
				sleeps.push(delayMs);
			},
		}).then(
			() => {
				throw new Error("expected runWorkerLoop to reject");
			},
			(error: unknown) => error,
		);

		expect(failure).toBeInstanceOf(ReconnectExhaustedError);
		expect((failure as Error).cause).toBe(lastDrop);
		// The last underlying error keeps its full detail through exhaustion.
		expect(((failure as Error).cause as { details: unknown }).details).toEqual(
			["denied"],
		);
		expect((failure as Error).message).toContain(
			"connection reset by load balancer",
		);
		expect(factoryCalls).toBe(1);
		expect(sleeps).toEqual([1]);
		expect(first.closed).toBe(true);
		expect(second.closed).toBe(true);
	});

	it("derives inter-cycle drop backoff from the config's own schedule with the cap applied", async () => {
		const drop = () =>
			serviceError(status.UNAVAILABLE, "14 UNAVAILABLE: stream reset");
		const sessions = [
			new ScriptedSession([{ kind: "throw", error: drop() }]),
			new ScriptedSession([{ kind: "throw", error: drop() }]),
			new ScriptedSession([{ kind: "throw", error: drop() }]),
			new ScriptedSession([{ kind: "throw", error: drop() }]),
			new ScriptedSession([{ kind: "throw", error: drop() }]),
		];
		const scheduleConfig: WorkerConfig = {
			...reconnectingConfig(),
			reconnect: { initialDelayMs: 1, maxDelayMs: 4, maxAttempts: 5 },
		};
		const sleeps: number[] = [];
		let factoryCalls = 0;

		const failure = await runWorkerLoop({
			config: scheduleConfig,
			session: sessions[0] as ScriptedSession,
			dispatcher: new SlowDispatcher(),
			sessionFactory: async () => {
				factoryCalls += 1;
				const session = sessions[factoryCalls];
				if (session === undefined) {
					throw new Error("session factory exhausted");
				}
				return session;
			},
			sleep: async (delayMs) => {
				sleeps.push(delayMs);
			},
		}).then(
			() => {
				throw new Error("expected runWorkerLoop to reject");
			},
			(error: unknown) => error,
		);

		// Four drops recovered with the doubling schedule capped at
		// maxDelayMs; the fifth drop exhausted the budget.
		expect(failure).toBeInstanceOf(ReconnectExhaustedError);
		expect(sleeps).toEqual([1, 2, 4, 4]);
		expect(factoryCalls).toBe(4);
	});

	it("counts a retryable result-replay failure against the drop budget", async () => {
		const drop = serviceError(
			status.UNAVAILABLE,
			"14 UNAVAILABLE: stream reset by transport",
		);
		const replayFailure = serviceError(
			status.UNAVAILABLE,
			"14 UNAVAILABLE: replay write lost the race with a second reset",
		);
		const tracker = new UnackedResultTracker();
		tracker.record({
			kind: "completed",
			workflowId: "workflow",
			activityId: "unacked-1",
			result: payload,
		});
		const first = new ScriptedSession([{ kind: "throw", error: drop }]);
		const second = new ReportFailingSession([], replayFailure);
		const third = new ScriptedSession([{ kind: "closed" }]);
		const replacements = [second, third];
		let factoryCalls = 0;

		const failure = await runWorkerLoop({
			config: reconnectingConfig(3),
			session: first,
			dispatcher: new SlowDispatcher(),
			tracker,
			sessionFactory: async () => {
				const session = replacements[factoryCalls];
				factoryCalls += 1;
				if (session === undefined) {
					throw new Error("session factory exhausted");
				}
				return session;
			},
			sleep: () => Promise.resolve(),
		}).then(
			() => {
				throw new Error("expected runWorkerLoop to reject");
			},
			(error: unknown) => error,
		);

		// Drop one: the stream reset. Drop two: the failed replay on the
		// second session (which was closed before re-entering the cycle).
		// The third session then received the replayed result before its own
		// clean close exhausted the budget — proving replay re-entry works
		// and shares the one cumulative budget.
		expect(failure).toBeInstanceOf(ReconnectExhaustedError);
		expect(factoryCalls).toBe(2);
		expect(second.closed).toBe(true);
		expect(third.reports).toEqual(["unacked-1"]);
		expect(third.closed).toBe(true);
	});

	it("propagates a non-retryable result-replay failure after closing the replacement session", async () => {
		const drop = serviceError(
			status.UNAVAILABLE,
			"14 UNAVAILABLE: stream reset by transport",
		);
		const denial = serviceError(
			status.PERMISSION_DENIED,
			"7 PERMISSION_DENIED: namespace 'payments' was revoked",
		);
		const tracker = new UnackedResultTracker();
		tracker.record({
			kind: "completed",
			workflowId: "workflow",
			activityId: "unacked-1",
			result: payload,
		});
		const first = new ScriptedSession([{ kind: "throw", error: drop }]);
		const second = new ReportFailingSession([], denial);

		await expect(
			runWorkerLoop({
				config: reconnectingConfig(3),
				session: first,
				dispatcher: new SlowDispatcher(),
				tracker,
				sessionFactory: async () => second,
				sleep: () => Promise.resolve(),
			}),
		).rejects.toBe(denial);

		expect(first.closed).toBe(true);
		expect(second.closed).toBe(true);
	});

	it("returns cleanly when an abort races the post-reconnect result replay", async () => {
		const drop = serviceError(
			status.UNAVAILABLE,
			"14 UNAVAILABLE: stream reset by transport",
		);
		const controller = new AbortController();
		const tracker = new UnackedResultTracker();
		tracker.record({
			kind: "completed",
			workflowId: "workflow",
			activityId: "unacked-1",
			result: payload,
		});
		const first = new ScriptedSession([{ kind: "throw", error: drop }]);
		// The abort lands inside the replay write itself: the shutdown
		// handler closed the just-published session, so the write fails the
		// way a real write-after-end does. Graceful shutdown must not turn
		// that into a rejected run.
		const second = new (class extends ScriptedSession {
			public override async reportResult(): Promise<void> {
				controller.abort();
				throw new Error("write after end");
			}
		})([]);

		await runWorkerLoop({
			config: reconnectingConfig(3),
			session: first,
			dispatcher: new SlowDispatcher(),
			tracker,
			sessionFactory: async () => second,
			sleep: () => Promise.resolve(),
			signal: controller.signal,
		});

		expect(second.closed).toBe(true);
		// The unacked result stays tracked for the caller; it was never
		// acknowledged by the server.
		expect(tracker.len()).toBe(1);
	});

	it("returns without serving when the signal is already aborted", async () => {
		const session = new ScriptedSession([{ kind: "task", task: task("1") }]);
		const controller = new AbortController();
		controller.abort();

		await runWorkerLoop({
			config: reconnectingConfig(),
			session,
			dispatcher: new SlowDispatcher(),
			sessionFactory: async () => session,
			sleep: () => Promise.resolve(),
			signal: controller.signal,
		});

		expect(session.handshakes).toBe(0);
		expect(session.reports).toEqual([]);
		expect(session.closed).toBe(true);
	});

	it("returns cleanly when an abort closes the session during initial registration", async () => {
		const controller = new AbortController();
		const session = new (class extends ScriptedSession {
			public override async register(): Promise<void> {
				controller.abort();
				throw new Error("write after end");
			}
		})([]);

		await runWorkerLoop({
			config: reconnectingConfig(),
			session,
			dispatcher: new SlowDispatcher(),
			sessionFactory: async () => session,
			sleep: () => Promise.resolve(),
			signal: controller.signal,
		});

		expect(session.handshakes).toBe(1);
		expect(session.reports).toEqual([]);
		expect(session.closed).toBe(true);
	});

	it("stops reconnecting on clean close once the shutdown signal aborts", async () => {
		const controller = new AbortController();
		// The abort fires while the stream is being served, immediately before
		// its clean close — after registration, so the loop reaches the
		// reconnect decision with the signal already aborted.
		const session = new (class extends ScriptedSession {
			public override async *receiveTasks(): AsyncIterable<WorkerSessionEvent> {
				controller.abort();
				yield { kind: "closed" };
			}
		})([]);
		let factoryCalls = 0;

		await runWorkerLoop({
			config: reconnectingConfig(),
			session,
			dispatcher: new SlowDispatcher(),
			sessionFactory: async () => {
				factoryCalls += 1;
				return new ScriptedSession([{ kind: "closed" }]);
			},
			sleep: () => Promise.resolve(),
			signal: controller.signal,
		});

		// The clean close returned instead of dialling a replacement: the
		// factory was never invoked, no drop budget was consumed, and the
		// session was never force-closed.
		expect(factoryCalls).toBe(0);
		expect(session.closed).toBe(false);
	});

	it("publishes the replacement session via onSessionChange and closes it on a shutdown raced with the reconnect", async () => {
		const unavailable = serviceError(
			status.UNAVAILABLE,
			"14 UNAVAILABLE: stream reset by transport",
		);
		const first = new ScriptedSession([{ kind: "throw", error: unavailable }]);
		const second = new ScriptedSession([{ kind: "closed" }]);
		const controller = new AbortController();
		const observed: WorkerSession[] = [];

		await runWorkerLoop({
			config: reconnectingConfig(),
			session: first,
			dispatcher: new SlowDispatcher(),
			sessionFactory: async () => second,
			sleep: () => Promise.resolve(),
			signal: controller.signal,
			onSessionChange: (session) => {
				observed.push(session);
				// Simulate a shutdown arriving while the reconnect was in
				// flight: the loop must close the fresh session and return
				// instead of serving it.
				controller.abort();
			},
		});

		expect(observed).toEqual([second]);
		expect(first.closed).toBe(true);
		expect(second.closed).toBe(true);
		expect(second.reports).toEqual([]);
	});

	it("propagates a retryable stream error when no session factory is configured", async () => {
		const unavailable = serviceError(
			status.UNAVAILABLE,
			"14 UNAVAILABLE: stream reset by transport",
		);
		const session = new ScriptedSession([
			{ kind: "throw", error: unavailable },
		]);

		await expect(
			runWorkerLoop({
				config: config(1),
				session,
				dispatcher: new SlowDispatcher(),
			}),
		).rejects.toBe(unavailable);

		expect(session.closed).toBe(true);
	});
});

type ScriptedStep =
	| { readonly kind: "task"; readonly task: ActivityTask }
	| { readonly kind: "closed" }
	| { readonly kind: "throw"; readonly error: Error };

class ScriptedSession implements WorkerSession {
	public readonly reports: string[] = [];
	public closed = false;
	public handshakes = 0;

	public constructor(private readonly script: readonly ScriptedStep[]) {}

	public handshake(): Promise<void> {
		this.handshakes += 1;
		return Promise.resolve();
	}

	public register(): Promise<void> {
		return Promise.resolve();
	}

	public async *receiveTasks(): AsyncIterable<WorkerSessionEvent> {
		for (const step of this.script) {
			if (step.kind === "throw") {
				throw step.error;
			}
			yield step;
		}
	}

	public async reportResult(
		_workflowId: string,
		activityId: string,
	): Promise<void> {
		this.reports.push(activityId);
	}

	public async reportFailure(
		_workflowId: string,
		activityId: string,
		_failure: ActivityFailure,
	): Promise<void> {
		this.reports.push(activityId);
	}

	public sendHeartbeat(): Promise<void> {
		return Promise.resolve();
	}

	public async close(): Promise<void> {
		this.closed = true;
	}
}

/**
 * Session whose report writes fail the way a dead transport's do, for the
 * post-reconnect result-replay failure paths.
 */
class ReportFailingSession extends ScriptedSession {
	public constructor(
		script: readonly ScriptedStep[],
		private readonly failure: Error,
	) {
		super(script);
	}

	public override async reportResult(): Promise<void> {
		throw this.failure;
	}

	public override async reportFailure(): Promise<void> {
		throw this.failure;
	}
}

function serviceError(code: number, message: string): Error {
	return Object.assign(new Error(message), {
		code,
		details: message,
		metadata: {},
	});
}

function reconnectingConfig(maxAttempts = 2): WorkerConfig {
	return {
		...config(1),
		reconnect: {
			initialDelayMs: 1,
			maxDelayMs: 2,
			maxAttempts,
		},
	};
}

function task(activityId: string): ActivityTask {
	return {
		workflowId: "workflow",
		activityId,
		activityType: "slow",
		input: payload,
		attempt: 1,
	};
}

function config(maxConcurrency: number): WorkerConfig {
	return {
		endpoint: "127.0.0.1:50051",
		taskQueue: "queue",
		identity: "identity",
		maxConcurrency,
		reconnect: {
			initialDelayMs: 1,
			maxDelayMs: 2,
			maxAttempts: 2,
		},
	};
}
