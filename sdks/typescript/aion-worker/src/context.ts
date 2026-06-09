import type { ActivityIdKey, Payload, WorkflowId } from "./session.js";

export type HeartbeatSender = (
	workflowId: WorkflowId,
	activityId: ActivityIdKey,
	progress?: Payload,
) => Promise<void>;

export interface ActivityContextOptions {
	readonly workflowId: WorkflowId;
	readonly activityId: ActivityIdKey;
	readonly attempt: number;
	readonly heartbeatSender: HeartbeatSender;
	readonly contentType?: string;
}

export class ActivityCancellationHandle {
	private cancelledValue = false;
	private readonly waiters = new Set<() => void>();

	public isCancelled(): boolean {
		return this.cancelledValue;
	}

	public cancelled(): Promise<void> {
		if (this.cancelledValue) {
			return Promise.resolve();
		}
		return new Promise<void>((resolve) => {
			this.waiters.add(resolve);
		});
	}

	public cancel(): void {
		if (this.cancelledValue) {
			return;
		}
		this.cancelledValue = true;
		const waiters = [...this.waiters];
		this.waiters.clear();
		for (const resolve of waiters) {
			resolve();
		}
	}
}

export class ActivityContext {
	public readonly activityId: ActivityIdKey;
	public readonly attempt: number;
	private readonly workflowId: WorkflowId;
	private readonly heartbeatSender: HeartbeatSender;
	private readonly cancellation: ActivityCancellationHandle;
	private readonly contentType: string;

	public constructor(
		options: ActivityContextOptions,
		cancellation: ActivityCancellationHandle = new ActivityCancellationHandle(),
	) {
		this.workflowId = options.workflowId;
		this.activityId = options.activityId;
		this.attempt = options.attempt;
		this.heartbeatSender = options.heartbeatSender;
		this.cancellation = cancellation;
		this.contentType = options.contentType ?? "application/json";
	}

	public async heartbeat(detail?: Payload | unknown): Promise<void> {
		await this.heartbeatSender(
			this.workflowId,
			this.activityId,
			detail === undefined ? undefined : payloadFromDetail(detail, this.contentType),
		);
	}

	public isCancelled(): boolean {
		return this.cancellation.isCancelled();
	}

	public cancelled(): Promise<void> {
		return this.cancellation.cancelled();
	}
}

function payloadFromDetail(detail: Payload | unknown, contentType: string): Payload {
	if (isPayload(detail)) {
		return detail;
	}
	return {
		contentType,
		bytes: new TextEncoder().encode(JSON.stringify(detail)),
	};
}

function isPayload(value: unknown): value is Payload {
	if (typeof value !== "object" || value === null) {
		return false;
	}
	const candidate = value as { contentType?: unknown; bytes?: unknown };
	return (
		typeof candidate.contentType === "string" &&
		candidate.bytes instanceof Uint8Array
	);
}
