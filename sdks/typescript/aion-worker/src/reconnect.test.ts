import { describe, expect, it } from "vitest";
import {
	reconnectWithBackoff,
	reReportUnacked,
	UnackedResultTracker,
} from "./reconnect.js";
import type {
	ActivityFailure,
	Payload,
	WorkerConfig,
	WorkerSession,
	WorkerSessionEvent,
} from "./session.js";

const payload: Payload = {
	contentType: "application/json",
	bytes: new Uint8Array([123, 125]),
};

class RecordingSession implements WorkerSession {
	public readonly events: string[];

	public constructor(events: string[]) {
		this.events = events;
	}

	public async handshake(config: WorkerConfig): Promise<void> {
		this.events.push(`handshake:${config.taskQueue}:${config.identity}`);
	}

	public async register(activityTypes: readonly string[]): Promise<void> {
		this.events.push(`register:${activityTypes.join(",")}`);
	}

	public async *receiveTasks(): AsyncIterable<WorkerSessionEvent> {
		this.events.push("receive");
		yield { kind: "closed" };
	}

	public async reportResult(
		_workflowId: string,
		activityId: string,
	): Promise<void> {
		this.events.push(`result:${activityId}`);
	}

	public async reportFailure(
		_workflowId: string,
		activityId: string,
		_failure: ActivityFailure,
	): Promise<void> {
		this.events.push(`failure:${activityId}`);
	}

	public async sendHeartbeat(): Promise<void> {}

	public async close(): Promise<void> {}
}

describe("reconnect", () => {
	it("re-reports unacknowledged completion before receiving new tasks", async () => {
		const events: string[] = [];
		const tracker = new UnackedResultTracker();
		tracker.record({
			kind: "completed",
			workflowId: "workflow",
			activityId: "1",
			result: payload,
		});

		const session = await reconnectWithBackoff(config(), ["charge"], {
			createSession: async () => new RecordingSession(events),
			sleep: async () => {},
		});
		await reReportUnacked(session, tracker);

		const iterator = session.receiveTasks()[Symbol.asyncIterator]();
		await iterator.next();

		expect(events).toEqual([
			"handshake:payments:worker-a",
			"register:charge",
			"result:1",
			"receive",
		]);
		expect(tracker.len()).toBe(1);
	});
});

function config(): WorkerConfig {
	return {
		endpoint: "127.0.0.1:50051",
		taskQueue: "payments",
		identity: "worker-a",
		maxConcurrency: 1,
		reconnect: {
			initialDelayMs: 1,
			maxDelayMs: 2,
			maxAttempts: 2,
		},
	};
}
