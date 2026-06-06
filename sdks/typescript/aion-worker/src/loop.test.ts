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

	public async handshake(): Promise<void> {}

	public async register(): Promise<void> {}

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

	public async sendHeartbeat(): Promise<void> {}

	public async close(): Promise<void> {}
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
				info: () => {},
				warn: () => {},
				error: (message, fields) => {
					warnings.push(`${message}:${String(fields?.retryable)}`);
				},
			},
		});

		expect(session.reports).toEqual(["1"]);
		expect(warnings).toEqual([
			"worker dispatcher threw unclassified error:true",
		]);
	});
});

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
