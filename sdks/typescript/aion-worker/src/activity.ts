import { ActivityCancellationHandle, ActivityContext } from "./context.js";
import { RetryableError, TerminalError } from "./errors.js";
import type { ActivityDispatcher, DispatchOutcome } from "./loop.js";
import type { WorkerLogger } from "./reconnect.js";
import type { ActivityFailure, ActivityTask, Payload, WorkerSession } from "./session.js";

export type ActivityHandler<I, O> = (
	input: I,
	ctx: ActivityContext,
) => Promise<O>;

/**
 * Type-erased execution form of a registered activity: raw payload in, raw
 * payload out. Decoding the input and encoding the output happen inside,
 * which is what lets heterogeneous activities share one registry.
 */
export type ErasedActivityRun = (
	input: Payload,
	ctx: ActivityContext,
) => Promise<Payload>;

/**
 * The registrable form of an activity. Deliberately type-erased — the
 * registry stores activities of arbitrary input/output types, and a typed
 * `handler: (input: I) => ...` property would be contravariant in `I` and
 * unassignable to a common element type. Instead the concrete types live
 * only inside the closure built by {@link defineActivity}, with the JSON
 * encode/decode boundary doing the conversion — mirroring how the engine's
 * events carry an opaque `Payload` rather than a generic parameter.
 */
export interface ActivityDefinition {
	readonly name: string;
	readonly run: ErasedActivityRun;
}

export interface ActivityRegistryOptions {
	readonly logger?: WorkerLogger;
	readonly heartbeatSender?: WorkerSession["sendHeartbeat"];
	readonly cancellationSink?: (handle: ActivityCancellationHandle) => void;
}

interface RegisteredActivity {
	readonly definition: ActivityDefinition;
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

	public register(definition: ActivityDefinition): void {
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
				dispatchActivity(task, definition.run, dependencies),
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

	/**
	 * Rebuilds this registry with heartbeats routed to `target`. The target
	 * only needs `sendHeartbeat`, so callers can pass either a raw session or
	 * a live-session router that always resolves the current transport (the
	 * public `Worker` does the latter, keeping heartbeats valid across
	 * reconnects).
	 */
	public withSession(
		target: Pick<WorkerSession, "sendHeartbeat">,
		options: Omit<ActivityRegistryOptions, "heartbeatSender"> = {},
	): ActivityRegistry {
		return new ActivityRegistry(this.definitions(), {
			...options,
			heartbeatSender: target.sendHeartbeat.bind(target),
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

/**
 * Builds a registrable {@link ActivityDefinition} from a fully typed handler.
 * The generic types exist only here: the returned definition closes over the
 * handler and erases `I`/`O` behind the JSON payload encode/decode boundary.
 */
export function defineActivity<I, O>(
	name: string,
	handler: ActivityHandler<I, O>,
): ActivityDefinition {
	if (name.length === 0) {
		throw new Error("activity name must not be empty");
	}
	return {
		name,
		run: async (input, ctx) => {
			const decoded = decodeJsonPayload<I>(input);
			const output = await handler(decoded, ctx);
			return encodeJsonPayload(output, input.contentType);
		},
	};
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

async function dispatchActivity(
	task: ActivityTask,
	run: ErasedActivityRun,
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
		const output = await run(task.input, ctx);
		return { kind: "completed", output };
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
