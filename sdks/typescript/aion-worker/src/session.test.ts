import { describe, expect, it } from "vitest";
import {
	decodePayload,
	encodePayload,
	GrpcWorkerSession,
	type WorkerConfig,
	type WorkerSession,
	type WorkerSessionEvent,
} from "./index.js";
import type { ServerToWorker, WorkerToServer } from "./proto/index.js";

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
		});

		await expect(session.register([])).rejects.toThrow(
			"worker registration must include at least one activity type",
		);
		expect(stream.messages).toEqual([]);
	});
});

class RecordingStream implements AsyncIterable<ServerToWorker> {
	public readonly messages: WorkerToServer[] = [];

	public write(
		message: WorkerToServer,
		callback?: (error?: Error | null) => void,
	): boolean {
		this.messages.push(message);
		callback?.();
		return true;
	}

	public end(): void {
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
