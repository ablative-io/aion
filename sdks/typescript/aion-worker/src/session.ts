import * as grpc from "@grpc/grpc-js";
import type {
	ActivityError,
	ActivityId,
	ActivityTask as WireActivityTask,
	Payload as WirePayload,
	RegisterAck as WireRegisterAck,
	ResultAck as WireResultAck,
	ServerToWorker,
	WorkerProtocolClient,
	WorkerToServer,
} from "./proto/index.js";

export type WorkerIdentity = string;

/**
 * Operator-supplied reconnect/backoff policy governing both session
 * establishment and the run loop's cumulative mid-run session-drop budget
 * of `maxAttempts`.
 *
 * Budget reset: the cumulative drop budget resets to zero once an
 * established session proves healthy — it served at least one task, or it
 * stayed connected longer than `maxDelayMs` (measured monotonically from
 * successful registration to the moment the stream ended or dropped;
 * post-drop draining of in-flight handlers never extends it). The cap is
 * the policy's own definition of the longest pause, so a session outliving
 * it is demonstrably past the flapping regime, and a served task proves
 * end-to-end health. A genuinely flapping server — no session ever serves a
 * task or outlives `maxDelayMs` — exhausts the budget after exactly
 * `maxAttempts` drops.
 *
 * Drains and clean closes: a server-announced drain (the wire
 * `DrainRequest` frame) is an unbudgeted drop — the worker finishes
 * in-flight work and redials after `initialDelayMs`; the drain
 * classification latches for the session, so even an abrupt end after the
 * frame stays drain-class. An *unannounced* clean stream close remains a
 * budgeted retryable drop: the worker redials through the same budgeted,
 * backed-off cycle, and only a persistent unannounced clean-close loop
 * exhausts the budget (surfacing `ReconnectExhaustedError` with a
 * `ServerClosedStreamError` cause).
 *
 * Shutdown during establishment or a backoff: shutdown wins promptly during
 * BOTH backoff phases — the session-establishment retries and the mid-run
 * drop backoffs — AND during an in-flight establishment attempt. Every SDK
 * races each backoff sleep and the whole dial/handshake/register chain
 * against the shutdown signal and never dials again once it fires (the Rust
 * worker selects shutdown around the entire establishment; this SDK and the
 * Python worker race the attempt and close an attempt abandoned to shutdown
 * when it eventually settles). The run outcome is aligned across the Rust,
 * Python, and TypeScript workers: a pending drain-class or clean-close drop
 * ends the run cleanly, while a pending error-class drop surfaces its error
 * — a supervisor sees "this worker was mid-fault" distinctly from "this
 * worker drained cleanly".
 */
export interface ReconnectConfig {
	readonly initialDelayMs: number;
	/**
	 * Backoff delay cap. Doubles as the session-health threshold for the
	 * drop-budget reset described on this type.
	 */
	readonly maxDelayMs: number;
	readonly maxAttempts: number;
}

export interface WorkerConfig {
	readonly endpoint: string;
	readonly taskQueue: string;
	readonly identity: WorkerIdentity;
	readonly maxConcurrency: number;
	readonly credentials?: grpc.ChannelCredentials;
	readonly channelOptions?: grpc.ChannelOptions;
	/**
	 * Reconnect/backoff policy. Required: the worker's run loop is
	 * reconnect-aware and refuses to operate without an operator-supplied
	 * budget (there are no SDK defaults), so a missing policy is a compile
	 * error rather than a runtime rejection.
	 */
	readonly reconnect: ReconnectConfig;
}

export interface Payload {
	readonly contentType: string;
	readonly bytes: Uint8Array;
}

export type WorkflowId = string;
export type ActivityIdKey = string;

export interface ActivityTask {
	readonly workflowId: WorkflowId;
	readonly activityId: ActivityIdKey;
	readonly activityType: string;
	readonly input: Payload;
	readonly attempt: number;
}

export interface ActivityFailure {
	readonly retryable: boolean;
	readonly message: string;
	readonly details?: Payload;
}

export interface WorkerRegistration {
	readonly taskQueue: string;
	readonly identity: WorkerIdentity;
	readonly activityTypes: readonly string[];
}

export type WorkerSessionEvent =
	| { readonly kind: "task"; readonly task: ActivityTask }
	/**
	 * The server consumed the identified `ActivityResult` frame; the worker
	 * may stop re-reporting it. Clears the matching unacked-tracker entry.
	 */
	| {
			readonly kind: "resultAck";
			readonly workflowId: WorkflowId;
			readonly activityId: ActivityIdKey;
	  }
	/**
	 * Server-initiated drain: the server is going away. The worker finishes
	 * in-flight work, stops expecting new tasks, and reconnects after the
	 * schedule's initial backoff without consuming drop budget.
	 */
	| { readonly kind: "drained" }
	| { readonly kind: "closed" };

/** Server-assigned registration facts carried by the `RegisterAck` frame. */
export interface RegisteredSessionInfo {
	readonly workerId: number;
	readonly namespace: string;
	readonly heartbeatWindowMs: number;
}

export interface WorkerSession {
	handshake(config: WorkerConfig): Promise<void>;
	register(activityTypes: readonly string[]): Promise<void>;
	receiveTasks(): AsyncIterable<WorkerSessionEvent>;
	reportResult(
		workflowId: WorkflowId,
		activityId: ActivityIdKey,
		result: Payload,
	): Promise<void>;
	reportFailure(
		workflowId: WorkflowId,
		activityId: ActivityIdKey,
		failure: ActivityFailure,
	): Promise<void>;
	sendHeartbeat(
		workflowId: WorkflowId,
		activityId: ActivityIdKey,
		progress?: Payload,
	): Promise<void>;
	close(): Promise<void>;
}

export type WorkerSessionFactory = (
	config: WorkerConfig,
) => Promise<WorkerSession>;

export interface GrpcClientFactory {
	create(
		endpoint: string,
		credentials: grpc.ChannelCredentials,
		options?: grpc.ChannelOptions,
	): WorkerProtocolClient;
}

export class GrpcWorkerSession implements WorkerSession {
	private readonly stream: ReturnType<WorkerProtocolClient["streamWorker"]>;
	/**
	 * The ONE iterator over the response stream: `register` consumes frame 1
	 * (the `RegisterAck`) from it and `receiveTasks` continues it, so no
	 * frame can be lost between the two phases.
	 */
	private readonly frames: AsyncIterator<ServerToWorker>;
	private registration?: WorkerRegistration;
	private config?: WorkerConfig;
	private registeredInfoValue?: RegisteredSessionInfo;
	private streamEnded = false;

	public constructor(client: WorkerProtocolClient) {
		this.stream = client.streamWorker();
		this.frames = this.stream[Symbol.asyncIterator]();
	}

	/** Server-assigned registration facts, available once registered. */
	public get registeredInfo(): RegisteredSessionInfo | undefined {
		return this.registeredInfoValue;
	}

	public async handshake(config: WorkerConfig): Promise<void> {
		validateWorkerConfig(config);
		this.config = config;
		this.registration = {
			taskQueue: config.taskQueue,
			identity: config.identity,
			activityTypes: [],
		};
	}

	/**
	 * Sends `RegisterWorker` and awaits the server's `RegisterAck` — the
	 * guaranteed first frame on the response stream. Registration succeeds
	 * only when the ack arrives; the wait is bounded by the reconnect
	 * policy's `maxDelayMs` (the operator's own definition of the longest
	 * tolerable pause). A timeout, a non-ack first frame, or a stream that
	 * ends before the ack is a retryable registration failure; denials fail
	 * the call with a gRPC error status exactly as before.
	 */
	public async register(activityTypes: readonly string[]): Promise<void> {
		const registeredTypes = validateActivityTypes(activityTypes);
		const registration = this.registration;
		const config = this.config;
		if (registration === undefined || config === undefined) {
			throw new Error("worker session must handshake before register");
		}
		this.registration = { ...registration, activityTypes: registeredTypes };
		await this.write({
			register: {
				namespace: registration.taskQueue,
				activityTypes: registeredTypes,
			},
		});
		const first = await withDeadline(
			this.frames.next(),
			config.reconnect.maxDelayMs,
			`server did not acknowledge registration within ${String(config.reconnect.maxDelayMs)}ms`,
		);
		if (first.done === true) {
			throw new Error(
				"server ended the stream before acknowledging registration",
			);
		}
		const ack = first.value.registerAck;
		if (ack === undefined) {
			throw new Error(
				"protocol violation: server sent a non-RegisterAck frame before acknowledging registration",
			);
		}
		this.registeredInfoValue = decodeRegisterAck(ack);
	}

	public async *receiveTasks(): AsyncIterable<WorkerSessionEvent> {
		for (;;) {
			const next = await this.frames.next();
			if (next.done === true) {
				break;
			}
			const message = next.value;
			if (message.task !== undefined) {
				yield { kind: "task", task: decodeTask(message.task) };
			} else if (message.resultAck !== undefined) {
				yield decodeResultAck(message.resultAck);
			} else if (message.drain !== undefined) {
				yield { kind: "drained" };
			} else if (message.registerAck !== undefined) {
				// The ack is consumed inside register(); a second one
				// mid-stream is a server ordering bug that must surface.
				throw new Error(
					"protocol violation: RegisterAck received after registration completed",
				);
			}
		}
		yield { kind: "closed" };
	}

	public async reportResult(
		workflowId: WorkflowId,
		activityId: ActivityIdKey,
		result: Payload,
	): Promise<void> {
		await this.write({
			result: {
				workflowId: encodeWorkflowId(workflowId),
				activityId: encodeActivityId(activityId),
				result: encodePayload(result),
			},
		});
	}

	public async reportFailure(
		workflowId: WorkflowId,
		activityId: ActivityIdKey,
		failure: ActivityFailure,
	): Promise<void> {
		await this.write({
			result: {
				workflowId: encodeWorkflowId(workflowId),
				activityId: encodeActivityId(activityId),
				error: encodeFailure(failure),
			},
		});
	}

	public async sendHeartbeat(
		workflowId: WorkflowId,
		activityId: ActivityIdKey,
		progress?: Payload,
	): Promise<void> {
		await this.write({
			heartbeat: {
				workflowId: encodeWorkflowId(workflowId),
				activityId: encodeActivityId(activityId),
				progress: progress === undefined ? undefined : encodePayload(progress),
			},
		});
	}

	/**
	 * Ends the underlying client stream. Explicitly idempotent: the worker
	 * loop's failure path and the worker's abort handler can both close the
	 * same session in one shutdown race, so only the first call ends the
	 * stream and every later call is a no-op — the contract is pinned here
	 * (and by test) rather than relying on Node tolerating a double `end()`.
	 */
	public async close(): Promise<void> {
		if (this.streamEnded) {
			return;
		}
		this.streamEnded = true;
		this.stream.end();
	}

	/**
	 * Writes one frame with a per-send deadline of the reconnect policy's
	 * `maxDelayMs`: a flush that outlives the operator's longest tolerable
	 * pause is, by that same definition, a dead session and surfaces as a
	 * retryable transport-shaped error instead of hanging the worker forever.
	 */
	private async write(message: WorkerToServer): Promise<void> {
		const config = this.config;
		if (config === undefined) {
			throw new Error("worker session must handshake before writing");
		}
		const flushed = new Promise<void>((resolve, reject) => {
			this.stream.write(message, (error?: Error | null) => {
				if (error === undefined || error === null) {
					resolve();
				} else {
					reject(error);
				}
			});
		});
		await withDeadline(
			flushed,
			config.reconnect.maxDelayMs,
			`worker stream send did not complete within ${String(config.reconnect.maxDelayMs)}ms`,
		);
	}
}

/**
 * Races a promise against a deadline timer; elapse rejects with a
 * transport-shaped `Error` (no gRPC denial code, so it stays retryable by
 * `isRetryableSessionError`). The timer is always cleared so a resolved
 * promise never leaves a dangling timeout keeping the process alive.
 */
async function withDeadline<T>(
	pending: Promise<T>,
	deadlineMs: number,
	message: string,
): Promise<T> {
	let timer: ReturnType<typeof setTimeout> | undefined;
	const elapsed = new Promise<never>((_resolve, reject) => {
		timer = setTimeout(() => {
			reject(new Error(message));
		}, deadlineMs);
	});
	try {
		return await Promise.race([pending, elapsed]);
	} finally {
		if (timer !== undefined) {
			clearTimeout(timer);
		}
	}
}

export async function connectGrpcWorkerSession(
	config: WorkerConfig,
	clientFactory: GrpcClientFactory,
): Promise<WorkerSession> {
	validateWorkerConfig(config);
	const credentials = config.credentials ?? grpc.credentials.createInsecure();
	const client = clientFactory.create(
		config.endpoint,
		credentials,
		config.channelOptions,
	);
	return new GrpcWorkerSession(client);
}

function validateWorkerConfig(config: WorkerConfig): void {
	if (config.endpoint.length === 0) {
		throw new Error("worker endpoint must not be empty");
	}
	if (config.taskQueue.length === 0) {
		throw new Error("worker taskQueue must not be empty");
	}
	if (config.identity.length === 0) {
		throw new Error("worker identity must not be empty");
	}
	if (!Number.isInteger(config.maxConcurrency) || config.maxConcurrency <= 0) {
		throw new Error("worker maxConcurrency must be a positive integer");
	}
}

function validateActivityTypes(
	activityTypes: readonly string[],
): readonly string[] {
	if (activityTypes.length === 0) {
		throw new Error(
			"worker registration must include at least one activity type",
		);
	}
	const normalizedTypes = [...activityTypes];
	for (const activityType of normalizedTypes) {
		if (activityType.length === 0) {
			throw new Error("worker activity type must not be empty");
		}
	}
	return normalizedTypes;
}

export function decodeTask(task: WireActivityTask): ActivityTask {
	if (task.workflowId === undefined || task.workflowId.uuid.length === 0) {
		throw new Error("activity task is missing workflow_id");
	}
	if (task.activityId === undefined) {
		throw new Error("activity task is missing activity_id");
	}
	if (task.activityType.length === 0) {
		throw new Error("activity task is missing activity_type");
	}
	if (task.input === undefined) {
		throw new Error("activity task is missing input payload");
	}
	const attempt = Number(task.attempt ?? 0);
	if (!Number.isInteger(attempt) || attempt <= 0) {
		// proto3 zero default = the producer failed to stamp the attempt.
		throw new Error(
			"activity task attempt is missing or zero (producer failed to stamp it)",
		);
	}
	return {
		workflowId: task.workflowId.uuid,
		activityId: activityIdToKey(task.activityId),
		activityType: task.activityType,
		input: decodePayload(task.input),
		attempt,
	};
}

function decodeResultAck(ack: WireResultAck): WorkerSessionEvent {
	if (ack.workflowId === undefined || ack.workflowId.uuid.length === 0) {
		throw new Error("result ack is missing workflow_id");
	}
	if (ack.activityId === undefined) {
		throw new Error("result ack is missing activity_id");
	}
	return {
		kind: "resultAck",
		workflowId: ack.workflowId.uuid,
		activityId: activityIdToKey(ack.activityId),
	};
}

function decodeRegisterAck(ack: WireRegisterAck): RegisteredSessionInfo {
	return {
		workerId: Number(ack.workerId ?? 0),
		namespace: ack.namespace ?? "",
		heartbeatWindowMs: Number(ack.heartbeatWindowMs ?? 0),
	};
}

export function decodePayload(payload: WirePayload): Payload {
	if (payload.contentType.length === 0) {
		throw new Error("payload is missing content_type");
	}
	return {
		contentType: payload.contentType,
		bytes: Uint8Array.from(payload.bytes),
	};
}

export function encodePayload(payload: Payload): WirePayload {
	if (payload.contentType.length === 0) {
		throw new Error("payload contentType must not be empty");
	}
	return {
		contentType: payload.contentType,
		bytes: Uint8Array.from(payload.bytes),
	};
}

export function activityIdToKey(activityId: ActivityId): ActivityIdKey {
	return String(activityId.sequencePosition);
}

function encodeWorkflowId(workflowId: WorkflowId): { readonly uuid: string } {
	if (workflowId.length === 0) {
		throw new Error("workflowId must not be empty");
	}
	return { uuid: workflowId };
}

function encodeActivityId(activityId: ActivityIdKey): {
	readonly sequencePosition: string;
} {
	if (activityId.length === 0) {
		throw new Error("activityId must not be empty");
	}
	return { sequencePosition: activityId };
}

function encodeFailure(failure: ActivityFailure): ActivityError {
	if (failure.message.length === 0) {
		throw new Error("activity failure message must not be empty");
	}
	return {
		kind: failure.retryable ? 1 : 2,
		message: failure.message,
		details:
			failure.details === undefined
				? undefined
				: encodePayload(failure.details),
	};
}
