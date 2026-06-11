import { status } from "@grpc/grpc-js";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
	defaultSleep,
	delayForAttempt,
	grpcStatusCode,
	isRetryableSessionError,
	ReconnectExhaustedError,
	reconnectWithBackoff,
	requireReconnectConfig,
	reReportUnacked,
	sleepUnlessAborted,
	UnackedResultTracker,
	type WorkerLogger,
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
	afterEach(() => {
		vi.useRealTimers();
	});

	it("keeps reports from distinct workflows that share a sequence position", () => {
		const tracker = new UnackedResultTracker();
		const failure: ActivityFailure = {
			retryable: false,
			message: "card declined",
		};
		tracker.record({
			kind: "completed",
			workflowId: "workflow-a",
			activityId: "3",
			result: payload,
		});
		tracker.record({
			kind: "failed",
			workflowId: "workflow-b",
			activityId: "3",
			failure,
		});

		// Same sequence position, different workflows: both entries coexist.
		expect(tracker.len()).toBe(2);
		expect(tracker.isEmpty()).toBe(false);
		expect(tracker.get("workflow-a", "3")?.kind).toBe("completed");
		expect(tracker.get("workflow-b", "3")?.kind).toBe("failed");
		expect(
			tracker.snapshot().map((report) => `${report.workflowId}:${report.activityId}`),
		).toEqual(["workflow-a:3", "workflow-b:3"]);

		// Acknowledging one workflow's report never touches the other's.
		tracker.acknowledge("workflow-a", "3");
		expect(tracker.get("workflow-a", "3")).toBeUndefined();
		expect(tracker.get("workflow-b", "3")?.kind).toBe("failed");
		expect(tracker.len()).toBe(1);

		// Acknowledging an absent pair is a no-op, not an error.
		tracker.acknowledge("workflow-a", "3");
		tracker.acknowledge("workflow-missing", "3");
		expect(tracker.len()).toBe(1);

		tracker.acknowledge("workflow-b", "3");
		expect(tracker.isEmpty()).toBe(true);
	});

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
			signal: undefined,
		});
		if (session === undefined) {
			throw new Error("expected reconnectWithBackoff to resolve a session");
		}
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
				signal: undefined,
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
				signal: undefined,
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

		const failure = await reconnectWithBackoff(config(), ["charge"], {
			createSession: async () => {
				attempts += 1;
				throw unavailable;
			},
			sleep: async (delayMs) => {
				sleeps.push(delayMs);
			},
			signal: undefined,
		}).then(
			() => {
				throw new Error("expected reconnectWithBackoff to reject");
			},
			(error: unknown) => error,
		);

		// Exhaustion is a classified error preserving the last underlying
		// failure (with its detail) as `cause`.
		expect(failure).toBeInstanceOf(ReconnectExhaustedError);
		expect((failure as Error).message).toBe(
			"worker reconnect attempts exhausted",
		);
		expect((failure as Error).cause).toBe(unavailable);
		expect(attempts).toBe(2);
		expect(sleeps).toEqual([1]);
		expect(isRetryableSessionError(unavailable)).toBe(true);
	});

	it("resolves undefined promptly when shutdown aborts during an establishment backoff", async () => {
		const unavailable = serviceError(
			status.UNAVAILABLE,
			"14 UNAVAILABLE: engine unreachable",
		);
		const controller = new AbortController();
		const sleeps: number[] = [];
		let attempts = 0;

		// The establishment-backoff sleep never resolves — it stands in for an
		// arbitrarily long delay — so only the abort race can end the wait.
		// The result must be undefined (the caller's clean-return contract,
		// mirroring the drop backoff) well before the backoff could elapse.
		const session = await reconnectWithBackoff(config(), ["charge"], {
			createSession: async () => {
				attempts += 1;
				throw unavailable;
			},
			sleep: (delayMs) => {
				sleeps.push(delayMs);
				setImmediate(() => {
					controller.abort();
				});
				return new Promise<void>(() => undefined);
			},
			signal: controller.signal,
		});

		expect(session).toBeUndefined();
		// Exactly the one pre-abort dial: shutdown never grows the session count.
		expect(attempts).toBe(1);
		expect(sleeps).toEqual([1]);
	});

	it("disarms the default backoff timer when the abort wins the sleep race", async () => {
		// B-1: the production defaultSleep must not leave its setTimeout armed
		// after the abort wins — a worker exiting by event-loop drain would
		// otherwise linger up to one max backoff after SIGTERM (a SIGKILL
		// window) even though the run loop returned promptly.
		vi.useFakeTimers();
		const controller = new AbortController();
		const sleeping = sleepUnlessAborted(defaultSleep, 60_000, controller.signal);
		expect(vi.getTimerCount()).toBe(1);
		controller.abort();
		await sleeping;
		expect(vi.getTimerCount()).toBe(0);
	});

	it("completes an unaborted default sleep and leaves no timer behind", async () => {
		// The clear-on-abort mechanism must not change legitimate sleeps: a
		// sleep that runs to completion resolves on schedule and the fired
		// timer is gone (cancel after completion is a no-op).
		vi.useFakeTimers();
		const controller = new AbortController();
		let slept = false;
		const sleeping = sleepUnlessAborted(
			defaultSleep,
			25,
			controller.signal,
		).then(() => {
			slept = true;
		});
		expect(vi.getTimerCount()).toBe(1);
		await vi.advanceTimersByTimeAsync(25);
		await sleeping;
		expect(slept).toBe(true);
		expect(vi.getTimerCount()).toBe(0);
	});

	it("resolves undefined promptly when shutdown aborts during a hung in-flight dial", async () => {
		// B-2 parity with the Rust worker's select around the ENTIRE
		// establishment: the dial never resolves — it stands in for a hung
		// connect — so only the abort race can end the wait. The result must
		// be undefined (the caller's clean-return contract) without waiting
		// for the transport's own connect behaviour.
		const controller = new AbortController();
		let attempts = 0;

		const session = await reconnectWithBackoff(config(), ["charge"], {
			createSession: () => {
				attempts += 1;
				setImmediate(() => {
					controller.abort();
				});
				return new Promise<WorkerSession>(() => undefined);
			},
			sleep: () => Promise.resolve(),
			signal: controller.signal,
		});

		expect(session).toBeUndefined();
		expect(attempts).toBe(1);
	});

	it("closes the session an abandoned dial resolves after shutdown won the race", async () => {
		// B-2: the losing establishment attempt keeps running in the
		// background; when it eventually resolves a session, the attached
		// continuation must close it so the transport never leaks.
		const controller = new AbortController();
		const events: string[] = [];
		let resolveDial: ((session: WorkerSession) => void) | undefined;

		const session = await reconnectWithBackoff(config(), ["charge"], {
			createSession: () =>
				new Promise<WorkerSession>((resolve) => {
					resolveDial = resolve;
					setImmediate(() => {
						controller.abort();
					});
				}),
			sleep: () => Promise.resolve(),
			signal: controller.signal,
		});

		expect(session).toBeUndefined();
		if (resolveDial === undefined) {
			throw new Error("expected the dial to have started");
		}
		resolveDial(new RecordingSession(events));
		// The abandoned attempt finishes its chain in the background and the
		// continuation closes the late session.
		await new Promise<void>((resolve) => {
			setImmediate(resolve);
		});
		await new Promise<void>((resolve) => {
			setImmediate(resolve);
		});
		expect(events).toEqual([
			"handshake:payments:worker-a",
			"register:charge",
			"close",
		]);
	});

	it("logs an abandoned dial that fails after shutdown instead of rejecting unhandled", async () => {
		const controller = new AbortController();
		const warnings: string[] = [];
		const logger: WorkerLogger = {
			info: () => undefined,
			warn: (message) => {
				warnings.push(message);
			},
			error: () => undefined,
		};
		let rejectDial: ((error: Error) => void) | undefined;

		const session = await reconnectWithBackoff(config(), ["charge"], {
			createSession: () =>
				new Promise<WorkerSession>((_resolve, reject) => {
					rejectDial = reject;
					setImmediate(() => {
						controller.abort();
					});
				}),
			sleep: () => Promise.resolve(),
			logger,
			signal: controller.signal,
		});

		expect(session).toBeUndefined();
		if (rejectDial === undefined) {
			throw new Error("expected the dial to have started");
		}
		rejectDial(new Error("dial torn down at shutdown"));
		await new Promise<void>((resolve) => {
			setImmediate(resolve);
		});
		// Swallow-with-log is acceptable ONLY because the worker is exiting;
		// an unhandled rejection here would fail the vitest run outright.
		expect(warnings).toContain(
			"worker session establishment abandoned at shutdown failed",
		);
	});

	it("resolves undefined without dialling when the signal is already aborted", async () => {
		const controller = new AbortController();
		controller.abort();
		let attempts = 0;

		const session = await reconnectWithBackoff(config(), ["charge"], {
			createSession: async () => {
				attempts += 1;
				return new RecordingSession([]);
			},
			sleep: () => Promise.resolve(),
			signal: controller.signal,
		});

		expect(session).toBeUndefined();
		expect(attempts).toBe(0);
	});

	it("requires a reconnect config and rejects non-positive values", () => {
		expect(() => requireReconnectConfig(undefined)).toThrow(
			"worker reconnect config is required",
		);
		expect(() =>
			requireReconnectConfig({
				initialDelayMs: 0,
				maxDelayMs: 2,
				maxAttempts: 2,
			}),
		).toThrow("worker reconnect initialDelayMs must be a positive integer");
		expect(() =>
			requireReconnectConfig({
				initialDelayMs: 1,
				maxDelayMs: 2,
				maxAttempts: 0,
			}),
		).toThrow("worker reconnect maxAttempts must be a positive integer");
		expect(() =>
			requireReconnectConfig({
				initialDelayMs: 4,
				maxDelayMs: 2,
				maxAttempts: 2,
			}),
		).toThrow("worker reconnect initialDelayMs must not exceed maxDelayMs");
	});

	it("derives the bounded exponential delay schedule from the config", () => {
		const reconnect = { initialDelayMs: 3, maxDelayMs: 10, maxAttempts: 6 };
		expect(delayForAttempt(reconnect, 1)).toBe(3);
		expect(delayForAttempt(reconnect, 2)).toBe(6);
		expect(delayForAttempt(reconnect, 3)).toBe(10);
		expect(delayForAttempt(reconnect, 4)).toBe(10);
		expect(() => delayForAttempt(reconnect, 0)).toThrow(
			"reconnect attempt must be a positive integer",
		);
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
			signal: undefined,
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
				signal: undefined,
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

		// connect-es style denial: numeric code with array details. Not a
		// grpc-js ServiceError shape, so it classifies retryable — acceptable
		// because the worker's bounded drop budget surfaces it (with detail
		// preserved as the exhaustion cause) instead of spinning forever.
		const arrayDetails = Object.assign(new Error("connect-es denial"), {
			code: status.PERMISSION_DENIED,
			details: [{ type: "namespace", debug: "denied" }],
		});
		expect(grpcStatusCode(arrayDetails)).toBeUndefined();
		expect(isRetryableSessionError(arrayDetails)).toBe(true);

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
