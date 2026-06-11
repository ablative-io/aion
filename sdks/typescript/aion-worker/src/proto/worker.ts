import type { ActivityId, Payload, WorkflowId } from "./common.js";

export const ActivityErrorKind = {
	ACTIVITY_ERROR_KIND_UNSPECIFIED: 0,
	ACTIVITY_ERROR_KIND_RETRYABLE: 1,
	ACTIVITY_ERROR_KIND_TERMINAL: 2,
} as const;

export type ActivityErrorKind =
	(typeof ActivityErrorKind)[keyof typeof ActivityErrorKind];

export interface RegisterWorker {
	readonly namespace: string;
	readonly activityTypes: readonly string[];
}

export interface ActivityTask {
	readonly workflowId?: WorkflowId;
	readonly activityId?: ActivityId;
	readonly activityType: string;
	readonly input?: Payload;
	/**
	 * One-based delivery attempt stamped by the dispatching engine seam.
	 * Zero/absent is malformed: the producer failed to stamp it.
	 */
	readonly attempt?: number;
}

/**
 * Positive registration acknowledgement — always the first frame on the
 * response stream. Denials flow exclusively as gRPC error statuses; there is
 * no negative counterpart frame.
 */
export interface RegisterAck {
	readonly workerId?: number | string;
	readonly namespace?: string;
	readonly heartbeatWindowMs?: number | string;
}

/**
 * Per-result acknowledgement: the server has consumed the identified
 * ActivityResult frame and the worker may stop re-reporting it.
 */
export interface ResultAck {
	readonly workflowId?: WorkflowId;
	readonly activityId?: ActivityId;
}

/**
 * Server-initiated drain: the server is going away. The worker finishes
 * in-flight work, stops expecting new tasks, and reconnects after the
 * schedule's initial backoff without consuming drop budget.
 */
export interface DrainRequest {}

export interface ActivityError {
	readonly kind: ActivityErrorKind;
	readonly message: string;
	readonly details?: Payload;
}

export interface ActivityResult {
	readonly workflowId?: WorkflowId;
	readonly activityId?: ActivityId;
	readonly result?: Payload;
	readonly error?: ActivityError;
}

export interface Heartbeat {
	readonly workflowId?: WorkflowId;
	readonly activityId?: ActivityId;
	readonly progress?: Payload;
}

export interface WorkerToServer {
	readonly register?: RegisterWorker;
	readonly result?: ActivityResult;
	readonly heartbeat?: Heartbeat;
}

export interface ServerToWorker {
	readonly task?: ActivityTask;
	readonly drain?: DrainRequest;
	readonly registerAck?: RegisterAck;
	readonly resultAck?: ResultAck;
}

export interface WorkerProtocolClient {
	streamWorker(): GrpcDuplexStream<WorkerToServer, ServerToWorker>;
}

export interface GrpcDuplexStream<Writable, Readable>
	extends AsyncIterable<Readable> {
	write(message: Writable, callback?: (error?: Error | null) => void): boolean;
	end(): void;
	on(event: "error", listener: (error: Error) => void): this;
	on(event: "end", listener: () => void): this;
	on(event: "data", listener: (message: Readable) => void): this;
}
