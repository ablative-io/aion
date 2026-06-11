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

class QueueSession implements WorkerSession {
	public readonly completed: Payload[] = [];
	public readonly failures: ActivityFailure[] = [];
	public readonly registrations: string[][] = [];
	private readonly events: WorkerSessionEvent[] = [];
	private resolver?: () => void;
	private closed = false;

	public handshake(): Promise<void> {
		return Promise.resolve();
	}

	public async register(activityTypes: readonly string[]): Promise<void> {
		this.registrations.push([...activityTypes]);
	}

	public async *receiveTasks(): AsyncIterable<WorkerSessionEvent> {
		for (;;) {
			const event = this.events.shift();
			if (event !== undefined) {
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

	public sendHeartbeat(): Promise<void> {
		return Promise.resolve();
	}

	public async close(): Promise<void> {
		this.closed = true;
		this.wake();
	}

	public push(event: WorkerSessionEvent): void {
		this.events.push(event);
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
		const session = new QueueSession();
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
		const session = new QueueSession();
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
		const run = worker.run();

		session.push({ kind: "task", task: task("increment", { value: 6 }) });
		session.push({ kind: "closed" });
		await run;

		expect(session.registrations).toEqual([["increment"]]);
		expect(session.failures).toEqual([]);
		expect(decode(session.completed[0] as Payload)).toEqual({ value: 7 });
	});
});

async function macrotaskTurns(turns: number): Promise<void> {
	for (let turn = 0; turn < turns; turn += 1) {
		await new Promise<void>((resolve) => {
			setTimeout(resolve, 0);
		});
	}
}

function task(activityType: string, input: unknown): ActivityTask {
	return {
		workflowId: "workflow",
		activityId: "activity",
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
	};
}
