import { describe, expect, it } from "vitest";
import {
	decodePayload,
	decodeTask,
	encodePayload,
	GrpcWorkerSession,
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

	it("decodes a wire task reading the attempt from the wire", () => {
		// Brief test 29: the attempt is stamped by the producer and read
		// verbatim — the consumer-side WIRE_DEFAULT_ATTEMPT parity hack is
		// deleted across all three SDKs.
		const wire: WireActivityTask = {
			workflowId: { uuid: "wf-1" },
			activityId: { sequencePosition: 7n },
			activityType: "charge",
			input: {
				contentType: "application/json",
				bytes: new Uint8Array([123, 125]),
			},
			attempt: 3,
		};

		const task = decodeTask(wire);

		expect(task.workflowId).toBe("wf-1");
		expect(task.activityId).toBe("7");
		expect(task.activityType).toBe("charge");
		expect(task.input).toEqual({
			contentType: "application/json",
			bytes: new Uint8Array([123, 125]),
		});
		expect(task.attempt).toBe(3);
	});

	it("rejects a wire task whose attempt is missing or zero", () => {
		const wire: WireActivityTask = {
			workflowId: { uuid: "wf-1" },
			activityId: { sequencePosition: 7n },
			activityType: "charge",
			input: {
				contentType: "application/json",
				bytes: new Uint8Array([123, 125]),
			},
		};

		expect(() => decodeTask(wire)).toThrow(
			"activity task attempt is missing or zero",
		);
		expect(() => decodeTask({ ...wire, attempt: 0 })).toThrow(
			"activity task attempt is missing or zero",
		);
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

/**
 * Scripted duplex stream for the ack-contract tests: serves the given
 * response frames in order, then ends (or hangs forever when
 * `hangAfterFrames` is set, for ack/write deadline coverage).
 */
class ScriptedStream implements AsyncIterable<ServerToWorker> {
	public readonly messages: WorkerToServer[] = [];
	public endCalls = 0;

	public constructor(
		private readonly frames: ServerToWorker[] = [],
		private readonly options: {
			readonly hangAfterFrames?: boolean;
			readonly neverFlushWrites?: boolean;
		} = {},
	) {}

	public write(
		message: WorkerToServer,
		callback?: (error?: Error | null) => void,
	): boolean {
		this.messages.push(message);
		if (this.options.neverFlushWrites !== true) {
			callback?.();
		}
		return true;
	}

	public end(): void {
		this.endCalls += 1;
	}

	public on(): this {
		return this;
	}

	public async *[Symbol.asyncIterator](): AsyncIterableIterator<ServerToWorker> {
		for (const frame of this.frames) {
			yield frame;
		}
		if (this.options.hangAfterFrames === true) {
			await new Promise<never>(() => undefined);
		}
	}
}

function sessionConfig(): WorkerConfig {
	return {
		endpoint: "127.0.0.1:50051",
		taskQueue: "payments",
		identity: "worker-a",
		maxConcurrency: 2,
		reconnect: { initialDelayMs: 5, maxDelayMs: 20, maxAttempts: 3 },
	};
}

function registerAckFrame(): ServerToWorker {
	return {
		registerAck: {
			workerId: 7,
			namespace: "payments",
			heartbeatWindowMs: 30_000,
		},
	};
}

describe("GrpcWorkerSession ack contract", () => {
	it("register completes only on RegisterAck and exposes its payload", async () => {
		const stream = new ScriptedStream([registerAckFrame()]);
		const session = new GrpcWorkerSession({ streamWorker: () => stream });
		await session.handshake(sessionConfig());

		await session.register(["charge"]);

		expect(session.registeredInfo).toEqual({
			workerId: 7,
			namespace: "payments",
			heartbeatWindowMs: 30_000,
		});
		expect(stream.messages[0]?.register?.activityTypes).toEqual(["charge"]);
	});

	it("register times out retryably when the server never acks", async () => {
		// Brief test 27: a never-acking server must time the ack wait out at
		// reconnect.maxDelayMs, never hang the worker.
		const stream = new ScriptedStream([], { hangAfterFrames: true });
		const session = new GrpcWorkerSession({ streamWorker: () => stream });
		await session.handshake(sessionConfig());

		await expect(session.register(["charge"])).rejects.toThrow(
			"did not acknowledge registration within 20ms",
		);
	});

	it("register rejects a non-ack first frame as a protocol violation", async () => {
		const stream = new ScriptedStream([
			{
				task: {
					workflowId: { uuid: "wf-1" },
					activityId: { sequencePosition: 1n },
					activityType: "charge",
					input: { contentType: "application/json", bytes: new Uint8Array() },
					attempt: 1,
				},
			},
		]);
		const session = new GrpcWorkerSession({ streamWorker: () => stream });
		await session.handshake(sessionConfig());

		await expect(session.register(["charge"])).rejects.toThrow(
			"protocol violation: server sent a non-RegisterAck frame",
		);
	});

	it("register fails retryably when the stream ends before the ack", async () => {
		const stream = new ScriptedStream([]);
		const session = new GrpcWorkerSession({ streamWorker: () => stream });
		await session.handshake(sessionConfig());

		await expect(session.register(["charge"])).rejects.toThrow(
			"server ended the stream before acknowledging registration",
		);
	});

	it("receiveTasks yields drained and resultAck events", async () => {
		// Brief test 27 (regression for the silent skip): the drain frame was
		// previously not even decodable and every non-task frame was dropped.
		const stream = new ScriptedStream([
			registerAckFrame(),
			{
				resultAck: {
					workflowId: { uuid: "wf-1" },
					activityId: { sequencePosition: 4n },
				},
			},
			{ drain: {} },
		]);
		const session = new GrpcWorkerSession({ streamWorker: () => stream });
		await session.handshake(sessionConfig());
		await session.register(["charge"]);

		const events: WorkerSessionEvent[] = [];
		for await (const event of session.receiveTasks()) {
			events.push(event);
		}

		expect(events).toEqual([
			{ kind: "resultAck", workflowId: "wf-1", activityId: "4" },
			{ kind: "drained" },
			{ kind: "closed" },
		]);
	});

	it("throws on a RegisterAck arriving mid-stream", async () => {
		const stream = new ScriptedStream([registerAckFrame(), registerAckFrame()]);
		const session = new GrpcWorkerSession({ streamWorker: () => stream });
		await session.handshake(sessionConfig());
		await session.register(["charge"]);

		const drain = async (): Promise<void> => {
			for await (const event of session.receiveTasks()) {
				void event;
			}
		};
		await expect(drain()).rejects.toThrow(
			"protocol violation: RegisterAck received after registration completed",
		);
	});

	it("write rejects at the deadline on a never-flushing stream", async () => {
		// Brief test 27: a send that outlives reconnect.maxDelayMs is a dead
		// session and must surface instead of hanging.
		const stream = new ScriptedStream([registerAckFrame()], {
			neverFlushWrites: true,
		});
		const session = new GrpcWorkerSession({ streamWorker: () => stream });
		await session.handshake(sessionConfig());

		await expect(
			session.reportResult("wf-1", "1", {
				contentType: "application/json",
				bytes: new Uint8Array(),
			}),
		).rejects.toThrow("worker stream send did not complete within 20ms");
	});
});
