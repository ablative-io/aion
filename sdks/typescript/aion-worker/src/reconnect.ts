import { status } from "@grpc/grpc-js";
import type {
	ActivityFailure,
	ActivityIdKey,
	Payload,
	ReconnectConfig,
	WorkerConfig,
	WorkerSession,
	WorkerSessionFactory,
	WorkflowId,
} from "./session.js";

/**
 * Deterministic server denials that no reconnect attempt can ever fix:
 * PERMISSION_DENIED (ungranted namespace) and UNAUTHENTICATED (rejected
 * credentials).
 */
export const NON_RETRYABLE_GRPC_STATUS_CODES: ReadonlySet<number> = new Set([
	status.PERMISSION_DENIED,
	status.UNAUTHENTICATED,
]);

/**
 * Extracts the numeric gRPC status code carried by an error or its `cause`
 * chain. `@grpc/grpc-js` surfaces server denials as `ServiceError`s with a
 * numeric `code`; Node transport errors carry string codes and are ignored.
 */
export function grpcStatusCode(error: unknown): number | undefined {
	const seen = new Set<object>();
	let current: unknown = error;
	while (
		typeof current === "object" &&
		current !== null &&
		!seen.has(current)
	) {
		seen.add(current);
		const code = (current as { readonly code?: unknown }).code;
		if (typeof code === "number") {
			return code;
		}
		current = (current as { readonly cause?: unknown }).cause;
	}
	return undefined;
}

/**
 * Returns false for PERMISSION_DENIED / UNAUTHENTICATED denials so the
 * reconnect loop surfaces them immediately instead of burning its attempt
 * budget; every other failure keeps the bounded backoff behaviour.
 */
export function isRetryableSessionError(error: unknown): boolean {
	const code = grpcStatusCode(error);
	return code === undefined || !NON_RETRYABLE_GRPC_STATUS_CODES.has(code);
}

export type PendingActivityReport =
	| {
			readonly kind: "completed";
			readonly workflowId: WorkflowId;
			readonly activityId: ActivityIdKey;
			readonly result: Payload;
	  }
	| {
			readonly kind: "failed";
			readonly workflowId: WorkflowId;
			readonly activityId: ActivityIdKey;
			readonly failure: ActivityFailure;
	  };

export class UnackedResultTracker {
	private readonly reports = new Map<ActivityIdKey, PendingActivityReport>();

	public record(report: PendingActivityReport): void {
		this.reports.set(report.activityId, report);
	}

	public acknowledge(activityId: ActivityIdKey): void {
		this.reports.delete(activityId);
	}

	public get(activityId: ActivityIdKey): PendingActivityReport | undefined {
		return this.reports.get(activityId);
	}

	public len(): number {
		return this.reports.size;
	}

	public isEmpty(): boolean {
		return this.reports.size === 0;
	}

	public snapshot(): readonly PendingActivityReport[] {
		return [...this.reports.values()];
	}
}

export interface ReconnectDependencies {
	readonly createSession: WorkerSessionFactory;
	readonly sleep?: (delayMs: number) => Promise<void>;
	readonly logger?: WorkerLogger;
}

export interface WorkerLogger {
	info(message: string, fields?: Record<string, unknown>): void;
	warn(message: string, fields?: Record<string, unknown>): void;
	error(message: string, fields?: Record<string, unknown>): void;
}

export async function reconnectWithBackoff(
	config: WorkerConfig,
	activityTypes: readonly string[],
	dependencies: ReconnectDependencies,
): Promise<WorkerSession> {
	const reconnect = requireReconnectConfig(config.reconnect);
	let delayMs = reconnect.initialDelayMs;
	let attempt = 1;
	let lastError: unknown;

	while (attempt <= reconnect.maxAttempts) {
		let session: WorkerSession | undefined;
		try {
			dependencies.logger?.info("worker reconnect attempt", { attempt });
			session = await dependencies.createSession(config);
			await session.handshake(config);
			await session.register(activityTypes);
			dependencies.logger?.info("worker reconnect succeeded", { attempt });
			return session;
		} catch (error) {
			lastError = error;
			await closeFailedSession(session, dependencies.logger);
			if (!isRetryableSessionError(error)) {
				dependencies.logger?.error(
					"worker reconnect denied by server; not retrying",
					{
						attempt,
						code: grpcStatusCode(error),
						message: error instanceof Error ? error.message : String(error),
					},
				);
				throw error;
			}
			dependencies.logger?.warn("worker reconnect attempt failed", {
				attempt,
				message: error instanceof Error ? error.message : String(error),
			});
			if (attempt === reconnect.maxAttempts) {
				break;
			}
			await (dependencies.sleep ?? defaultSleep)(delayMs);
			delayMs = nextDelay(delayMs, reconnect.maxDelayMs);
			attempt += 1;
		}
	}

	throw new Error("worker reconnect attempts exhausted", { cause: lastError });
}

/**
 * Closes a session whose handshake or registration failed so its transport
 * resources are released before the next attempt (or before the failure is
 * surfaced on the fail-fast path). A close failure is secondary: it is
 * logged and never allowed to mask the original connection error, matching
 * the Rust and Python workers' semantics.
 */
async function closeFailedSession(
	session: WorkerSession | undefined,
	logger: WorkerLogger | undefined,
): Promise<void> {
	if (session === undefined) {
		return;
	}
	try {
		await session.close();
	} catch (closeError) {
		logger?.warn("failed to close unsuccessful worker session", {
			message:
				closeError instanceof Error ? closeError.message : String(closeError),
		});
	}
}

export async function reReportUnacked(
	session: WorkerSession,
	tracker: UnackedResultTracker,
	logger?: WorkerLogger,
): Promise<void> {
	for (const report of tracker.snapshot()) {
		logger?.info("worker re-reporting unacknowledged activity", {
			activityId: report.activityId,
			kind: report.kind,
		});
		if (report.kind === "completed") {
			await session.reportResult(
				report.workflowId,
				report.activityId,
				report.result,
			);
		} else {
			await session.reportFailure(
				report.workflowId,
				report.activityId,
				report.failure,
			);
		}
	}
}

export function requireReconnectConfig(
	reconnect: ReconnectConfig | undefined,
): ReconnectConfig {
	if (reconnect === undefined) {
		throw new Error("worker reconnect config is required");
	}
	if (
		!Number.isInteger(reconnect.initialDelayMs) ||
		reconnect.initialDelayMs <= 0
	) {
		throw new Error(
			"worker reconnect initialDelayMs must be a positive integer",
		);
	}
	if (!Number.isInteger(reconnect.maxDelayMs) || reconnect.maxDelayMs <= 0) {
		throw new Error("worker reconnect maxDelayMs must be a positive integer");
	}
	if (!Number.isInteger(reconnect.maxAttempts) || reconnect.maxAttempts <= 0) {
		throw new Error("worker reconnect maxAttempts must be a positive integer");
	}
	if (reconnect.initialDelayMs > reconnect.maxDelayMs) {
		throw new Error(
			"worker reconnect initialDelayMs must not exceed maxDelayMs",
		);
	}
	return reconnect;
}

function nextDelay(delayMs: number, maxDelayMs: number): number {
	return Math.min(delayMs * 2, maxDelayMs);
}

async function defaultSleep(delayMs: number): Promise<void> {
	await new Promise<void>((resolve) => {
		setTimeout(resolve, delayMs);
	});
}
