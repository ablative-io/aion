import { ActivityCancellationHandle, ActivityContext } from "./context.js";
import { RetryableError, TerminalError } from "./errors.js";
import type { ActivityDispatcher, DispatchOutcome } from "./loop.js";
import type { WorkerLogger } from "./reconnect.js";
import type { ActivityFailure, ActivityTask, Payload, WorkerSession } from "./session.js";

export type ActivityHandler<I, O> = (
	input: I,
	ctx: ActivityContext,
) => Promise<O>;

export interface ActivityDefinition<I = unknown, O = unknown> {
	readonly name: string;
	readonly handler: ActivityHandler<I, O>;
}

export interface ActivityRegistryOptions {
	readonly logger?: WorkerLogger;
	readonly heartbeatSender?: WorkerSession["sendHeartbeat"];
	readonly cancellationSink?: (handle: ActivityCancellationHandle) => void;
}

interface RegisteredActivity<I = unknown, O = unknown> {
	readonly definition: ActivityDefinition<I, O>;
	readonly dispatch: (
		task: ActivityTask,
		dependencies: DispatchDependencies,
	) => Promise<DispatchOutcome>;
}

interface DispatchDependencies {
	readonly logger?: WorkerLogger;
	readonly heartbeatSender: WorkerSession["sendHeartbeat"];
	readonly cancellationSink?: (handle: ActivityCancellationHandle) => void;
	readonly cancellationReleased?: (handle: ActivityCancellationHandle) => void;
}

export class ActivityRegistry implements ActivityDispatcher {
	private readonly activities = new Map<string, RegisteredActivity>();
	private readonly logger?: WorkerLogger;
	private readonly heartbeatSender: WorkerSession["sendHeartbeat"];
	private readonly cancellationSink?: (handle: ActivityCancellationHandle) => void;
	private readonly activeCancellations = new Set<ActivityCancellationHandle>();

	public constructor(
		activities: readonly ActivityDefinition[] = [],
		options: ActivityRegistryOptions = {},
	) {
		this.logger = options.logger;
		this.heartbeatSender = options.heartbeatSender ?? noopHeartbeat;
		this.cancellationSink = options.cancellationSink;
		for (const activity of activities) {
			this.register(activity);
		}
	}

	public register<I, O>(definition: ActivityDefinition<I, O>): void {
		if (definition.name.length === 0) {
			throw new Error("activity name must not be empty");
		}
		if (this.activities.has(definition.name)) {
			throw new Error(
				`activity '${definition.name}' is already registered`,
			);
		}
		this.activities.set(definition.name, {
			definition,
			dispatch: async (task, dependencies) =>
				dispatchActivity(task, definition.handler, dependencies),
		});
	}

	public activityTypes(): readonly string[] {
		return [...this.activities.keys()].sort();
	}

	public async dispatch(task: ActivityTask): Promise<DispatchOutcome> {
		const activity = this.activities.get(task.activityType);
		if (activity === undefined) {
			return {
				kind: "failed",
				failure: {
					retryable: false,
					message: `activity '${task.activityType}' is not registered`,
				},
			};
		}
		return activity.dispatch(task, {
			logger: this.logger,
			heartbeatSender: this.heartbeatSender,
			cancellationSink: (handle) => {
				this.activeCancellations.add(handle);
				this.cancellationSink?.(handle);
			},
			cancellationReleased: (handle) => {
				this.activeCancellations.delete(handle);
			},
		});
	}

	public withSession(
		session: WorkerSession,
		options: Omit<ActivityRegistryOptions, "heartbeatSender"> = {},
	): ActivityRegistry {
		return new ActivityRegistry(this.definitions(), {
			...options,
			heartbeatSender: session.sendHeartbeat.bind(session),
		});
	}

	public cancelAll(): void {
		for (const cancellation of this.activeCancellations) {
			cancellation.cancel();
		}
	}

	private definitions(): readonly ActivityDefinition[] {
		return [...this.activities.values()].map(
			(activity) => activity.definition,
		);
	}
}

export function defineActivity<I, O>(
	name: string,
	handler: ActivityHandler<I, O>,
): ActivityDefinition<I, O> {
	if (name.length === 0) {
		throw new Error("activity name must not be empty");
	}
	return { name, handler };
}

export function decodeJsonPayload<I>(payload: Payload): I {
	const json = new TextDecoder().decode(payload.bytes);
	return JSON.parse(json) as I;
}

export function encodeJsonPayload<O>(value: O, contentType: string): Payload {
	return {
		contentType,
		bytes: new TextEncoder().encode(JSON.stringify(value)),
	};
}

async function dispatchActivity<I, O>(
	task: ActivityTask,
	handler: ActivityHandler<I, O>,
	dependencies: DispatchDependencies,
): Promise<DispatchOutcome> {
	const cancellation = new ActivityCancellationHandle();
	dependencies.cancellationSink?.(cancellation);
	const ctx = new ActivityContext(
		{
			workflowId: task.workflowId,
			activityId: task.activityId,
			attempt: task.attempt,
			heartbeatSender: dependencies.heartbeatSender,
			contentType: task.input.contentType,
		},
		cancellation,
	);
	try {
		const input = decodeJsonPayload<I>(task.input);
		const output = await handler(input, ctx);
		return {
			kind: "completed",
			output: encodeJsonPayload(output, task.input.contentType),
		};
	} catch (error) {
		return {
			kind: "failed",
			failure: classifyFailure(error, task.input.contentType, dependencies.logger),
		};
	} finally {
		dependencies.cancellationReleased?.(cancellation);
	}
}

function classifyFailure(
	error: unknown,
	contentType: string,
	logger: WorkerLogger | undefined,
): ActivityFailure {
	if (error instanceof RetryableError) {
		return {
			retryable: true,
			message: error.message,
			details: detailPayload(error.details, contentType),
		};
	}
	if (error instanceof TerminalError) {
		return {
			retryable: false,
			message: error.message,
			details: detailPayload(error.details, contentType),
		};
	}
	const message = error instanceof Error ? error.message : String(error);
	logger?.warn("activity handler threw unclassified error", {
		message,
		retryable: true,
	});
	return {
		retryable: true,
		message,
		details: error instanceof Error ? undefined : detailPayload(error, contentType),
	};
}

function detailPayload(detail: unknown, contentType: string): Payload | undefined {
	if (detail === undefined) {
		return undefined;
	}
	if (isPayload(detail)) {
		return detail;
	}
	return encodeJsonPayload(detail, contentType);
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

function noopHeartbeat(): Promise<void> {
	return Promise.resolve();
}
