import { status } from "@grpc/grpc-js";
import { describe, expect, it } from "vitest";
import {
	type ActivityDispatcher,
	type DispatchOutcome,
	runWorkerLoop,
} from "./loop.js";
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

		// The UNAVAILABLE stream error fed the bounded reconnect path: one
		// failed attempt slept and a second attempt produced the working
		// session whose task was served before its own stream denial ended
		// the loop fail-fast.
		expect(factoryCalls).toBe(2);
		expect(sleeps).toEqual([1]);
		expect(first.closed).toBe(true);
		expect(second.reports).toEqual(["after-reconnect"]);
		expect(second.closed).toBe(true);
	});

	it("still reconnects on a clean stream close without surfacing an error", async () => {
		const denialToEndTest = serviceError(
			status.PERMISSION_DENIED,
			"7 PERMISSION_DENIED: namespace 'payments' is not granted",
		);
		const first = new ScriptedSession([{ kind: "closed" }]);
		// The second session's iterator completes without yielding `closed`,
		// covering the `done: true` clean-completion path as well.
		const second = new ScriptedSession([{ kind: "task", task: task("1") }]);
		let factoryCalls = 0;

		await expect(
			runWorkerLoop({
				config: reconnectingConfig(),
				session: first,
				dispatcher: new SlowDispatcher(),
				sessionFactory: async () => {
					factoryCalls += 1;
					if (factoryCalls <= 2) {
						return factoryCalls === 1 ? second : first;
					}
					throw denialToEndTest;
				},
				sleep: () => Promise.resolve(),
			}),
		).rejects.toBe(denialToEndTest);

		// Clean closes reconnected (factory reached attempt three) and were
		// never treated as stream errors: no session was force-closed.
		expect(factoryCalls).toBe(3);
		expect(second.reports).toEqual(["1"]);
		expect(first.closed).toBe(false);
		expect(second.closed).toBe(false);
	});

	it("stops reconnecting on clean close once the shutdown signal aborts", async () => {
		const session = new ScriptedSession([{ kind: "closed" }]);
		const controller = new AbortController();
		controller.abort();
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
		// factory was never invoked and the session was never force-closed.
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

	public constructor(private readonly script: readonly ScriptedStep[]) {}

	public handshake(): Promise<void> {
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

function serviceError(code: number, message: string): Error {
	return Object.assign(new Error(message), {
		code,
		details: message,
		metadata: {},
	});
}

function reconnectingConfig(): WorkerConfig {
	return {
		...config(1),
		reconnect: {
			initialDelayMs: 1,
			maxDelayMs: 2,
			maxAttempts: 2,
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
	};
}
