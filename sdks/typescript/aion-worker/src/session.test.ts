import { describe, expect, it } from "vitest";
import {
	decodePayload,
	decodeTask,
	encodePayload,
	GrpcWorkerSession,
	WIRE_DEFAULT_ATTEMPT,
	type WorkerConfig,
	type WorkerSession,
	type WorkerSessionEvent,
} from "./index.js";
import type {
	ActivityTask as WireActivityTask,
	ServerToWorker,
	WorkerToServer,
} from "./proto/index.js";

class FakeSession implements WorkerSession {
	public handshakes: WorkerConfig[] = [];
	public registrations: string[][] = [];

	public async handshake(config: WorkerConfig): Promise<void> {
		this.handshakes.push(config);
	}

	public async register(activityTypes: readonly string[]): Promise<void> {
		this.registrations.push([...activityTypes]);
	}

	public async *receiveTasks(): AsyncIterable<WorkerSessionEvent> {
		yield { kind: "closed" };
	}

	public reportResult(): Promise<void> {
		return Promise.resolve();
	}

	public reportFailure(): Promise<void> {
		return Promise.resolve();
	}

	public sendHeartbeat(): Promise<void> {
		return Promise.resolve();
	}

	public close(): Promise<void> {
		return Promise.resolve();
	}
}

describe("WorkerSession", () => {
	it("carries configured task queue, identity, and activity names", async () => {
		const session = new FakeSession();
		const config: WorkerConfig = {
			endpoint: "127.0.0.1:50051",
			taskQueue: "payments",
			identity: "worker-a",
			maxConcurrency: 2,
			reconnect: { initialDelayMs: 100, maxDelayMs: 1_000, maxAttempts: 5 },
		};

		await session.handshake(config);
		await session.register(["charge", "refund"]);

		expect(session.handshakes).toHaveLength(1);
		expect(session.handshakes[0]?.taskQueue).toBe("payments");
		expect(session.handshakes[0]?.identity).toBe("worker-a");
		expect(session.registrations).toEqual([["charge", "refund"]]);
	});

	it("preserves payload content type on encode/decode", () => {
		const encoded = encodePayload({
			contentType: "application/json",
			bytes: new Uint8Array([123, 125]),
		});

		expect(decodePayload(encoded)).toEqual({
			contentType: "application/json",
			bytes: new Uint8Array([123, 125]),
		});
	});

	it("decodes a wire task with the documented wire-default attempt", () => {
		// The aion-proto ActivityTask carries no attempt field; the decoded
		// task must report the documented WIRE_DEFAULT_ATTEMPT constant
		// (parity with the Rust and Python workers), never an ad-hoc literal.
		const wire: WireActivityTask = {
			workflowId: { uuid: "wf-1" },
			activityId: { sequencePosition: 7n },
			activityType: "charge",
			input: {
				contentType: "application/json",
				bytes: new Uint8Array([123, 125]),
			},
		};

		const task = decodeTask(wire);

		expect(task.workflowId).toBe("wf-1");
		expect(task.activityId).toBe("7");
		expect(task.activityType).toBe("charge");
		expect(task.input).toEqual({
			contentType: "application/json",
			bytes: new Uint8Array([123, 125]),
		});
		expect(WIRE_DEFAULT_ATTEMPT).toBe(1);
		expect(task.attempt).toBe(WIRE_DEFAULT_ATTEMPT);
	});

	it("rejects empty activity registrations", async () => {
		const stream = new RecordingStream();
		const session = new GrpcWorkerSession({
			streamWorker: () => stream,
		});
		await session.handshake({
			endpoint: "127.0.0.1:50051",
			taskQueue: "payments",
			identity: "worker-a",
			maxConcurrency: 2,
			reconnect: { initialDelayMs: 100, maxDelayMs: 1_000, maxAttempts: 5 },
		});

		await expect(session.register([])).rejects.toThrow(
			"worker registration must include at least one activity type",
		);
		expect(stream.messages).toEqual([]);
	});

	it("close is idempotent: a double close ends the stream exactly once", async () => {
		// The worker loop's failure path and the worker's abort handler can
		// both close the same session in one shutdown race; the contract is
		// that only the first close ends the stream.
		const stream = new RecordingStream();
		const session = new GrpcWorkerSession({
			streamWorker: () => stream,
		});

		await session.close();
		await session.close();
		await session.close();

		expect(stream.endCalls).toBe(1);
	});
});

class RecordingStream implements AsyncIterable<ServerToWorker> {
	public readonly messages: WorkerToServer[] = [];
	public endCalls = 0;

	public write(
		message: WorkerToServer,
		callback?: (error?: Error | null) => void,
	): boolean {
		this.messages.push(message);
		callback?.();
		return true;
	}

	public end(): void {
		this.endCalls += 1;
		this.messages.push({});
	}

	public on(): this {
		return this;
	}

	public async *[Symbol.asyncIterator](): AsyncIterableIterator<ServerToWorker> {
		const message = undefined as ServerToWorker | undefined;
		if (message !== undefined) {
			yield message;
		}
	}
}
