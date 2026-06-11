import { status } from "@grpc/grpc-js";
import { describe, expect, it } from "vitest";
import {
	grpcStatusCode,
	isRetryableSessionError,
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

	public sendHeartbeat(): Promise<void> {
		return Promise.resolve();
	}

	public async close(): Promise<void> {
		this.events.push("close");
	}
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
			sleep: () => Promise.resolve(),
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

	it("stops after one attempt when registration is permission denied", async () => {
		const denial = serviceError(
			status.PERMISSION_DENIED,
			"7 PERMISSION_DENIED: namespace 'payments' is not granted",
		);
		const events: string[] = [];
		const sleeps: number[] = [];
		let attempts = 0;

		await expect(
			reconnectWithBackoff(config(), ["charge"], {
				createSession: async () => {
					attempts += 1;
					return new DeniedRegisterSession(events, denial);
				},
				sleep: async (delayMs) => {
					sleeps.push(delayMs);
				},
			}),
		).rejects.toBe(denial);

		expect(attempts).toBe(1);
		expect(sleeps).toEqual([]);
		expect(events).toEqual([
			"handshake:payments:worker-a",
			"register-denied",
			"close",
		]);
		expect(grpcStatusCode(denial)).toBe(status.PERMISSION_DENIED);
		expect(isRetryableSessionError(denial)).toBe(false);
	});

	it("stops after one attempt when the connection is unauthenticated", async () => {
		const denial = serviceError(
			status.UNAUTHENTICATED,
			"16 UNAUTHENTICATED: worker credentials were rejected",
		);
		const sleeps: number[] = [];
		let attempts = 0;

		await expect(
			reconnectWithBackoff(config(), ["charge"], {
				createSession: async () => {
					attempts += 1;
					throw denial;
				},
				sleep: async (delayMs) => {
					sleeps.push(delayMs);
				},
			}),
		).rejects.toBe(denial);

		expect(attempts).toBe(1);
		expect(sleeps).toEqual([]);
		expect(isRetryableSessionError(denial)).toBe(false);
	});

	it("still retries transport unavailability up to maxAttempts with backoff", async () => {
		const unavailable = serviceError(
			status.UNAVAILABLE,
			"14 UNAVAILABLE: engine unreachable",
		);
		const sleeps: number[] = [];
		let attempts = 0;

		const rejection = expect(
			reconnectWithBackoff(config(), ["charge"], {
				createSession: async () => {
					attempts += 1;
					throw unavailable;
				},
				sleep: async (delayMs) => {
					sleeps.push(delayMs);
				},
			}),
		).rejects;
		await rejection.toThrowError("worker reconnect attempts exhausted");

		expect(attempts).toBe(2);
		expect(sleeps).toEqual([1]);
		expect(isRetryableSessionError(unavailable)).toBe(true);
	});

	it("closes the failed session before the next attempt on retryable failures", async () => {
		const unavailable = serviceError(
			status.UNAVAILABLE,
			"14 UNAVAILABLE: registration stream reset",
		);
		const events: string[] = [];
		let attempts = 0;

		await reconnectWithBackoff(config(), ["charge"], {
			createSession: async () => {
				attempts += 1;
				events.push(`create:${attempts}`);
				if (attempts === 1) {
					return new FailingRegisterSession(events, unavailable);
				}
				return new RecordingSession(events);
			},
			sleep: async () => {
				events.push("sleep");
			},
		});

		expect(attempts).toBe(2);
		expect(events).toEqual([
			"create:1",
			"handshake:payments:worker-a",
			"register-unavailable",
			"close",
			"sleep",
			"create:2",
			"handshake:payments:worker-a",
			"register:charge",
		]);
	});

	it("closes the session on fail-fast denial without masking the denial when close throws", async () => {
		const denial = serviceError(
			status.PERMISSION_DENIED,
			"7 PERMISSION_DENIED: namespace 'payments' is not granted",
		);
		const closeFailure = new Error("transport already torn down");
		const events: string[] = [];
		const warnings: Array<{ message: string; detail?: unknown }> = [];
		let attempts = 0;

		await expect(
			reconnectWithBackoff(config(), ["charge"], {
				createSession: async () => {
					attempts += 1;
					return new ThrowingCloseSession(events, denial, closeFailure);
				},
				sleep: () => Promise.resolve(),
				logger: {
					info: () => undefined,
					warn: (message, fields) => {
						warnings.push({ message, detail: fields?.message });
					},
					error: () => undefined,
				},
			}),
		).rejects.toBe(denial);

		expect(attempts).toBe(1);
		expect(events).toEqual([
			"handshake:payments:worker-a",
			"register-denied",
			"close-throws",
		]);
		expect(warnings).toEqual([
			{
				message: "failed to close unsuccessful worker session",
				detail: "transport already torn down",
			},
		]);
	});

	it("ignores numeric codes on errors that are not gRPC-shaped", () => {
		const bareCode = Object.assign(new Error("application failure"), {
			code: status.PERMISSION_DENIED,
		});
		expect(grpcStatusCode(bareCode)).toBeUndefined();
		expect(isRetryableSessionError(bareCode)).toBe(true);

		const bareUnauthenticatedCode = Object.assign(
			new Error("another application failure"),
			{ code: status.UNAUTHENTICATED },
		);
		expect(grpcStatusCode(bareUnauthenticatedCode)).toBeUndefined();
		expect(isRetryableSessionError(bareUnauthenticatedCode)).toBe(true);

		const numericDetails = Object.assign(new Error("wrong details type"), {
			code: status.PERMISSION_DENIED,
			details: 7,
		});
		expect(grpcStatusCode(numericDetails)).toBeUndefined();
		expect(isRetryableSessionError(numericDetails)).toBe(true);

		const notAnError = {
			code: status.PERMISSION_DENIED,
			details: "shaped but not an Error instance",
			metadata: {},
		};
		expect(grpcStatusCode(notAnError)).toBeUndefined();
		expect(isRetryableSessionError(notAnError)).toBe(true);
	});

	it("classifies a real ServiceError shape as a deterministic denial", () => {
		const denial = serviceError(
			status.PERMISSION_DENIED,
			"7 PERMISSION_DENIED: namespace 'payments' is not granted",
		);
		expect(grpcStatusCode(denial)).toBe(status.PERMISSION_DENIED);
		expect(isRetryableSessionError(denial)).toBe(false);
	});

	it("skips a non-gRPC numeric code and finds the real ServiceError deeper in the cause chain", () => {
		const denial = serviceError(
			status.PERMISSION_DENIED,
			"7 PERMISSION_DENIED: namespace 'payments' is not granted",
		);
		const wrapper = Object.assign(
			new Error("wrapper with unrelated numeric code", { cause: denial }),
			{ code: 999 },
		);
		expect(grpcStatusCode(wrapper)).toBe(status.PERMISSION_DENIED);
		expect(isRetryableSessionError(wrapper)).toBe(false);
	});

	it("finds the gRPC status code through an error cause chain", () => {
		const denial = serviceError(
			status.PERMISSION_DENIED,
			"7 PERMISSION_DENIED: namespace 'payments' is not granted",
		);
		const wrapped = new Error("worker stream failed", { cause: denial });

		expect(grpcStatusCode(wrapped)).toBe(status.PERMISSION_DENIED);
		expect(isRetryableSessionError(wrapped)).toBe(false);
		expect(grpcStatusCode(new Error("plain failure"))).toBeUndefined();
		expect(isRetryableSessionError(new Error("plain failure"))).toBe(true);
	});
});

class DeniedRegisterSession extends RecordingSession {
	private readonly denial: Error;

	public constructor(events: string[], denial: Error) {
		super(events);
		this.denial = denial;
	}

	public override async register(): Promise<void> {
		this.events.push("register-denied");
		throw this.denial;
	}
}

class FailingRegisterSession extends RecordingSession {
	private readonly failure: Error;

	public constructor(events: string[], failure: Error) {
		super(events);
		this.failure = failure;
	}

	public override async register(): Promise<void> {
		this.events.push("register-unavailable");
		throw this.failure;
	}
}

class ThrowingCloseSession extends DeniedRegisterSession {
	private readonly closeFailure: Error;

	public constructor(events: string[], denial: Error, closeFailure: Error) {
		super(events, denial);
		this.closeFailure = closeFailure;
	}

	public override async close(): Promise<void> {
		this.events.push("close-throws");
		throw this.closeFailure;
	}
}

function serviceError(code: number, message: string): Error {
	return Object.assign(new Error(message), {
		code,
		details: message,
		metadata: undefined,
	});
}

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
