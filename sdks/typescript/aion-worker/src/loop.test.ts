import { status } from "@grpc/grpc-js";
import { describe, expect, it } from "vitest";
import {
	type ActivityDispatcher,
	type DispatchOutcome,
	runWorkerLoop,
} from "./loop.js";
import {
	ReconnectExhaustedError,
	ServerClosedStreamError,
	UnackedResultTracker,
} from "./reconnect.js";
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
				now: () => 0,
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
		// covering the `done: true` clean-completion path as well. No session
		// serves a task and the frozen clock keeps lifetimes at zero, so no
		// budget reset can fire: this is the flapping clean-close regime.
		const second = new ScriptedSession([]);
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
			now: () => 0,
		}).then(
			() => {
				throw new Error("expected runWorkerLoop to reject");
			},
			(error: unknown) => error,
		);

		// Each clean close re-entered the reconnect cycle, consumed one unit
		// of the shared drop budget, and backed off on the config's own
		// schedule; the third clean close exhausted the budget at exactly
		// maxAttempts and surfaced a classified error whose cause is the
		// typed clean-close drop (instanceof-checkable, like Rust's
		// CleanCloseExhausted and Python's ServerClosedStreamError).
		expect(failure).toBeInstanceOf(ReconnectExhaustedError);
		expect((failure as Error).message).toContain(
			"worker session drop budget exhausted after 3 dropped sessions",
		);
		expect((failure as Error).cause).toBeInstanceOf(ServerClosedStreamError);
		expect(((failure as Error).cause as Error).message).toBe(
			"worker receive stream closed cleanly while a session factory was configured",
		);
		expect(factoryCalls).toBe(2);
		expect(sleeps).toEqual([1, 2]);
		expect(second.reports).toEqual([]);
		// Replaced sessions are closed before their replacements are dialled.
		expect(first.closed).toBe(true);
		expect(second.closed).toBe(true);
		expect(third.closed).toBe(true);
	});

	it("reconnects after a clean close, re-registers, replays unacked results, and keeps serving", async () => {
		const denial = serviceError(
			status.PERMISSION_DENIED,
			"7 PERMISSION_DENIED: namespace 'queue' was revoked",
		);
		const tracker = new UnackedResultTracker();
		// The first session serves one task (reported but never acknowledged)
		// and then closes cleanly; the second session must see the replayed
		// unacked result before serving its own task. The deterministic
		// denial then ends the run fail-fast.
		const first = new ScriptedSession([
			{ kind: "task", task: task("1") },
			{ kind: "closed" },
		]);
		const second = new ScriptedSession([
			{ kind: "task", task: task("2") },
			{ kind: "throw", error: denial },
		]);
		let factoryCalls = 0;

		await expect(
			runWorkerLoop({
				config: reconnectingConfig(),
				session: first,
				dispatcher: new SlowDispatcher(),
				tracker,
				sessionFactory: async () => {
					factoryCalls += 1;
					return second;
				},
				sleep: () => Promise.resolve(),
				now: () => 0,
			}),
		).rejects.toBe(denial);

		// The clean close redialled through the budgeted cycle: the worker
		// re-registered its activity types on the replacement session,
		// re-reported the unacknowledged backlog ("1") before serving the new
		// task ("2"), and both sessions were closed.
		expect(factoryCalls).toBe(1);
		expect(first.reports).toEqual(["1"]);
		expect(first.closed).toBe(true);
		expect(second.handshakes).toBe(1);
		expect(second.registrations).toEqual([["slow"]]);
		expect(second.reports).toEqual(["1", "2"]);
		expect(second.closed).toBe(true);
	});

	it("resets the drop budget each time a session serves a task", async () => {
		const drop = () =>
			serviceError(status.UNAVAILABLE, "14 UNAVAILABLE: stream reset");
		const denial = serviceError(
			status.PERMISSION_DENIED,
			"7 PERMISSION_DENIED: namespace 'queue' was revoked",
		);
		const servingSession = (activityId: string) =>
			new ScriptedSession([
				{ kind: "task", task: task(activityId) },
				{ kind: "throw", error: drop() },
			]);
		const initial = servingSession("1");
		const replacements = [
			servingSession("2"),
			servingSession("3"),
			servingSession("4"),
			servingSession("5"),
			new ScriptedSession([{ kind: "throw", error: denial }]),
		];
		const sleeps: number[] = [];
		let factoryCalls = 0;

		await expect(
			runWorkerLoop({
				config: reconnectingConfig(2),
				session: initial,
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
				// Frozen clock: only the served-task rule can reset the budget.
				now: () => 0,
			}),
		).rejects.toBe(denial);

		// Five sessions each served a task before dropping. With
		// maxAttempts=2 the second drop would have exhausted the budget
		// without the reset rule; instead every served task reset it, the
		// worker kept recovering (every backoff restarted at the first-step
		// delay), and only the deterministic denial ended the run fail-fast.
		expect(factoryCalls).toBe(5);
		expect(sleeps).toEqual([1, 1, 1, 1, 1]);
	});

	it("resets the drop budget when a session outlives the max backoff delay", async () => {
		const drop = () =>
			serviceError(status.UNAVAILABLE, "14 UNAVAILABLE: stream reset");
		// now() is read once at each session's registration and once at its
		// drop. Session two "survives" 10ms against maxDelayMs=2ms; the
		// others drop instantly.
		const clockReads = [0, 0, 0, 10, 10, 10];
		let reads = 0;
		const initial = new ScriptedSession([{ kind: "throw", error: drop() }]);
		const replacements = [
			new ScriptedSession([{ kind: "throw", error: drop() }]),
			new ScriptedSession([{ kind: "throw", error: drop() }]),
		];
		let factoryCalls = 0;

		const failure = await runWorkerLoop({
			config: reconnectingConfig(2),
			session: initial,
			dispatcher: new SlowDispatcher(),
			sessionFactory: async () => {
				const session = replacements[factoryCalls];
				factoryCalls += 1;
				if (session === undefined) {
					throw new Error("session factory exhausted");
				}
				return session;
			},
			sleep: () => Promise.resolve(),
			now: () => {
				const value = clockReads[reads];
				reads += 1;
				return value ?? 10;
			},
		}).then(
			() => {
				throw new Error("expected runWorkerLoop to reject");
			},
			(error: unknown) => error,
		);

		// Drop one consumed the first budget unit. The second session served
		// no tasks but outlived maxDelayMs, so its drop restarted the count
		// at one. The third session's instant drop was the second post-reset
		// unit and exhausted maxAttempts=2 — proving exactly one unit was
		// consumed before the reset. Without the reset the run would have
		// ended after a single reconnect.
		expect(failure).toBeInstanceOf(ReconnectExhaustedError);
		expect(factoryCalls).toBe(2);
	});

	it("does not reset the drop budget when only the post-drop drain outlives the max backoff", async () => {
		const drop = () =>
			serviceError(status.UNAVAILABLE, "14 UNAVAILABLE: stream reset");
		// Virtual clock: the dispatched handler "runs" for 100ms — far past
		// maxDelayMs=2 — but only during the post-drop drain. The gate is
		// released via a macrotask scheduled just before the stream throws,
		// so it opens strictly after the loop captures the stream-end
		// timestamp (a microtask continuation) and strictly before the drain
		// settles.
		let t = 0;
		let releaseDispatch = (): void => undefined;
		const dispatchGate = new Promise<void>((resolve) => {
			releaseDispatch = resolve;
		});
		const second = new (class extends ScriptedSession {
			public override async *receiveTasks(): AsyncIterable<WorkerSessionEvent> {
				yield { kind: "task", task: task("drained") };
				setImmediate(() => {
					t = 100;
					releaseDispatch();
				});
				throw drop();
			}

			public override async reportResult(): Promise<void> {
				// The report fails, so the drained task never counts as served
				// and only the (mis)measured lifetime could reset the budget.
				throw drop();
			}
		})([]);
		const gatedDispatcher: ActivityDispatcher = {
			activityTypes: () => ["slow"],
			dispatch: async () => {
				await dispatchGate;
				return { kind: "completed", output: payload };
			},
		};
		const initial = new ScriptedSession([{ kind: "throw", error: drop() }]);
		const replacements = [
			second,
			new ScriptedSession([{ kind: "throw", error: drop() }]),
		];
		let factoryCalls = 0;

		const failure = await runWorkerLoop({
			// maxConcurrency 2 keeps the receive loop reading (and observing
			// the drop) while the gated handler holds the first slot.
			config: config(2),
			session: initial,
			dispatcher: gatedDispatcher,
			sessionFactory: async () => {
				const session = replacements[factoryCalls];
				factoryCalls += 1;
				if (session === undefined) {
					throw new Error("session factory exhausted");
				}
				return session;
			},
			sleep: () => Promise.resolve(),
			now: () => t,
		}).then(
			() => {
				throw new Error("expected runWorkerLoop to reject");
			},
			(error: unknown) => error,
		);

		// Drop one consumed the first budget unit. The second session dropped
		// at t=0 with its handler still in flight; the drain finished at
		// t=100 (past maxDelayMs=2) but connected time is measured to the
		// stream end, so no reset fired and the second drop exhausted
		// maxAttempts=2. Measured to the end of the drain, the budget would
		// have reset and a third session would have been dialled.
		expect(failure).toBeInstanceOf(ReconnectExhaustedError);
		expect(factoryCalls).toBe(1);
		expect(second.closed).toBe(true);
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
			// Frozen clock: no time-based reset, so exhaustion happens at
			// exactly maxAttempts drops.
			now: () => 0,
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
			now: () => 0,
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
			now: () => 0,
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

	it("re-reports both workflows' outcomes after a drop when their sequence positions collide", async () => {
		const drop = serviceError(
			status.UNAVAILABLE,
			"14 UNAVAILABLE: stream reset by transport",
		);
		// Two concurrent workflows whose activities share sequence position
		// "7": activity ids are per-workflow positions, so the tracker must
		// key by (workflowId, activityId) — keyed by activity id alone, the
		// second record overwrites the first and one computed outcome is
		// silently never replayed.
		const tracker = new UnackedResultTracker();
		const first = new WorkflowQualifiedSession([
			{ kind: "task", task: workflowTask("workflow-a", "7") },
			{ kind: "task", task: workflowTask("workflow-b", "7") },
			{ kind: "throw", error: drop },
		]);
		const second = new WorkflowQualifiedSession([{ kind: "closed" }]);
		const dispatcher: ActivityDispatcher = {
			activityTypes: () => ["slow"],
			dispatch: async (activityTask) =>
				activityTask.workflowId === "workflow-a"
					? { kind: "completed", output: payload }
					: {
							kind: "failed",
							failure: { retryable: false, message: "card declined" },
						},
		};

		const failure = await runWorkerLoop({
			config: { ...config(2), reconnect: reconnectingConfig().reconnect },
			session: first,
			dispatcher,
			tracker,
			sessionFactory: async () => second,
			sleep: () => Promise.resolve(),
			now: () => 0,
		}).then(
			() => {
				throw new Error("expected runWorkerLoop to reject");
			},
			(error: unknown) => error,
		);

		// The run itself ends by exhausting the drop budget on the second
		// session's clean close — the assertions that matter are the replay's.
		expect(failure).toBeInstanceOf(ReconnectExhaustedError);
		// Both outcomes were recorded and stayed tracked across the drop:
		// neither workflow's report overwrote the other's.
		expect(tracker.len()).toBe(2);
		expect(tracker.get("workflow-a", "7")?.kind).toBe("completed");
		expect(tracker.get("workflow-b", "7")?.kind).toBe("failed");
		// Both were re-reported on the replacement session. Under
		// activity-id-only keying, workflow-b's record replaces workflow-a's,
		// so the new session would see exactly one replayed report.
		expect([...second.qualifiedReports].sort()).toEqual([
			"failure:workflow-b:7",
			"result:workflow-a:7",
		]);
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

	it("returns promptly when shutdown aborts during a drop backoff", async () => {
		const controller = new AbortController();
		const drop = serviceError(
			status.UNAVAILABLE,
			"14 UNAVAILABLE: stream reset by transport",
		);
		const session = new ScriptedSession([{ kind: "throw", error: drop }]);
		const sleeps: number[] = [];
		let factoryCalls = 0;

		// The backoff sleep never resolves — it stands in for an arbitrarily
		// long delay — so only the abort race can end the wait. The loop must
		// return cleanly (the TS shutdown contract) without dialling a
		// replacement, well before the backoff could elapse.
		await runWorkerLoop({
			config: reconnectingConfig(3),
			session,
			dispatcher: new SlowDispatcher(),
			sessionFactory: async () => {
				factoryCalls += 1;
				return new ScriptedSession([{ kind: "closed" }]);
			},
			sleep: (delayMs) => {
				sleeps.push(delayMs);
				setImmediate(() => {
					controller.abort();
				});
				return new Promise<void>(() => undefined);
			},
			signal: controller.signal,
			now: () => 0,
		});

		expect(sleeps).toEqual([1]);
		expect(factoryCalls).toBe(0);
		expect(session.closed).toBe(true);
	});

	it("rejects with the denial when a server denial races an abort during registration", async () => {
		const denial = serviceError(
			status.PERMISSION_DENIED,
			"7 PERMISSION_DENIED: namespace 'queue' is not granted",
		);
		const controller = new AbortController();
		const errors: Array<Record<string, unknown> | undefined> = [];
		// The abort lands while the genuine denial is in flight: the denial is
		// deterministic and must reject the run so the supervisor learns of it
		// now — not after restarting a worker that exited cleanly.
		const session = new (class extends ScriptedSession {
			public override async register(): Promise<void> {
				controller.abort();
				throw denial;
			}
		})([]);

		await expect(
			runWorkerLoop({
				config: reconnectingConfig(),
				session,
				dispatcher: new SlowDispatcher(),
				sessionFactory: async () => session,
				sleep: () => Promise.resolve(),
				signal: controller.signal,
				logger: {
					info: () => undefined,
					warn: () => undefined,
					error: (_message, fields) => {
						errors.push(fields);
					},
				},
			}),
		).rejects.toBe(denial);

		expect(session.closed).toBe(true);
		expect(errors).toEqual([
			{ code: status.PERMISSION_DENIED, message: denial.message },
		]);
	});

	it("rejects with the denial when a server denial races an abort during result replay", async () => {
		const drop = serviceError(
			status.UNAVAILABLE,
			"14 UNAVAILABLE: stream reset by transport",
		);
		const denial = serviceError(
			status.PERMISSION_DENIED,
			"7 PERMISSION_DENIED: namespace 'queue' was revoked",
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
		const second = new (class extends ScriptedSession {
			public override async reportResult(): Promise<void> {
				controller.abort();
				throw denial;
			}
		})([]);

		await expect(
			runWorkerLoop({
				config: reconnectingConfig(3),
				session: first,
				dispatcher: new SlowDispatcher(),
				tracker,
				sessionFactory: async () => second,
				sleep: () => Promise.resolve(),
				signal: controller.signal,
			}),
		).rejects.toBe(denial);

		expect(first.closed).toBe(true);
		expect(second.closed).toBe(true);
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
		// session was closed on the way out so the shutdown path never leaks
		// its transport.
		expect(factoryCalls).toBe(0);
		expect(session.closed).toBe(true);
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
	public readonly registrations: string[][] = [];
	public closed = false;
	public handshakes = 0;

	public constructor(private readonly script: readonly ScriptedStep[]) {}

	public handshake(): Promise<void> {
		this.handshakes += 1;
		return Promise.resolve();
	}

	public register(activityTypes: readonly string[] = []): Promise<void> {
		this.registrations.push([...activityTypes]);
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
 * Session that records reports qualified by workflow id, for the
 * multi-workflow replay paths where the bare activity id is ambiguous.
 */
class WorkflowQualifiedSession extends ScriptedSession {
	public readonly qualifiedReports: string[] = [];

	public override async reportResult(
		workflowId: string,
		activityId: string,
	): Promise<void> {
		this.qualifiedReports.push(`result:${workflowId}:${activityId}`);
	}

	public override async reportFailure(
		workflowId: string,
		activityId: string,
		_failure: ActivityFailure,
	): Promise<void> {
		this.qualifiedReports.push(`failure:${workflowId}:${activityId}`);
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
	return workflowTask("workflow", activityId);
}

function workflowTask(workflowId: string, activityId: string): ActivityTask {
	return {
		workflowId,
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
