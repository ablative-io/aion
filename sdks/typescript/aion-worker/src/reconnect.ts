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
 * chain. Only gRPC-shaped errors are trusted: `@grpc/grpc-js` surfaces
 * server denials as `ServiceError`s (`StatusObject & Error`), so a numeric
 * `code` counts only when it sits on an `Error` that also carries the
 * `StatusObject` `details` string. A bare numeric `code` on an unrelated
 * error is never treated as a gRPC status (Node transport errors carry
 * string codes; arbitrary application errors may carry numeric ones), and
 * the walk continues through `cause` in case a real `ServiceError` sits
 * deeper in the chain.
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
		const code = serviceErrorCode(current);
		if (code !== undefined) {
			return code;
		}
		current = (current as { readonly cause?: unknown }).cause;
	}
	return undefined;
}

/**
 * Returns the status code when `candidate` matches the `@grpc/grpc-js`
 * `ServiceError` shape: an `Error` instance whose `code` is a number and
 * whose `details` is a string, with `metadata` (when present) an object —
 * `callErrorFromStatus` assigns the full `StatusObject` onto an `Error`,
 * and `PartialStatusObject` permits `metadata` to be absent or null.
 *
 * Trade-off: a denial that is NOT shaped like a grpc-js `ServiceError` — a
 * connect-es style error whose `details` is an array, or a cross-realm
 * `Error` that fails the `instanceof` check — is not recognised here and
 * therefore classifies as retryable. That is deliberate: trusting looser
 * shapes would let arbitrary application errors carrying a numeric `code`
 * masquerade as deterministic denials and kill the worker fail-fast. Such
 * errors instead consume the worker's bounded reconnect/drop budget; when
 * the budget exhausts, the surfaced {@link ReconnectExhaustedError} carries
 * the last underlying error (with its full detail) as `cause`, so the
 * misclassification costs bounded retry time — never an unbounded spin and
 * never a swallowed error.
 */
function serviceErrorCode(candidate: object): number | undefined {
	if (!(candidate instanceof Error)) {
		return undefined;
	}
	const shaped = candidate as Error & {
		readonly code?: unknown;
		readonly details?: unknown;
		readonly metadata?: unknown;
	};
	if (typeof shaped.code !== "number" || typeof shaped.details !== "string") {
		return undefined;
	}
	if (
		shaped.metadata !== undefined &&
		shaped.metadata !== null &&
		typeof shaped.metadata !== "object"
	) {
		return undefined;
	}
	return shaped.code;
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

/**
 * Tracks reported activity outcomes until the engine acknowledges them.
 *
 * Entries are keyed by workflow id and then activity id: activity ids are
 * sequence positions scoped to a single workflow, so reports from distinct
 * workflows legitimately share a bare position and must never replace one
 * another (parity with the Rust worker's `pending_report_key` tuple and the
 * Python worker's `(workflow uuid, sequence position)` key). The composite
 * key is a nested map — each level is keyed by the exact identifier value,
 * so no string-encoding of the pair exists that two distinct
 * (workflowId, activityId) pairs could collide on.
 */
export class UnackedResultTracker {
	private readonly reports = new Map<
		WorkflowId,
		Map<ActivityIdKey, PendingActivityReport>
	>();

	public record(report: PendingActivityReport): void {
		let workflowReports = this.reports.get(report.workflowId);
		if (workflowReports === undefined) {
			workflowReports = new Map<ActivityIdKey, PendingActivityReport>();
			this.reports.set(report.workflowId, workflowReports);
		}
		workflowReports.set(report.activityId, report);
	}

	public acknowledge(workflowId: WorkflowId, activityId: ActivityIdKey): void {
		const workflowReports = this.reports.get(workflowId);
		if (workflowReports === undefined) {
			return;
		}
		workflowReports.delete(activityId);
		if (workflowReports.size === 0) {
			this.reports.delete(workflowId);
		}
	}

	public get(
		workflowId: WorkflowId,
		activityId: ActivityIdKey,
	): PendingActivityReport | undefined {
		return this.reports.get(workflowId)?.get(activityId);
	}

	public len(): number {
		let total = 0;
		for (const workflowReports of this.reports.values()) {
			total += workflowReports.size;
		}
		return total;
	}

	public isEmpty(): boolean {
		return this.reports.size === 0;
	}

	public snapshot(): readonly PendingActivityReport[] {
		const reports: PendingActivityReport[] = [];
		for (const workflowReports of this.reports.values()) {
			reports.push(...workflowReports.values());
		}
		return reports;
	}
}

/**
 * A started backoff sleep whose underlying timer can be torn down. The
 * production {@link defaultSleep} returns one so that when shutdown wins the
 * race in {@link sleepUnlessAborted} the armed `setTimeout` is cleared —
 * otherwise a worker that exits by event-loop drain lingers up to one max
 * backoff after SIGTERM (a SIGKILL window at process level) even though the
 * run loop returned promptly. `cancel` must tolerate being called after the
 * sleep completed (clearing a fired timer is a no-op).
 */
export interface SleepHandle {
	readonly done: Promise<void>;
	cancel(): void;
}

/**
 * Injectable backoff-sleep seam. A plain promise-returning sleep (the shape
 * every test sleep uses) is accepted unchanged — it simply has no timer to
 * tear down — while a {@link SleepHandle}-returning sleep additionally
 * exposes cancellation so an abort can disarm the timer it started.
 */
export type BackoffSleep = (delayMs: number) => Promise<void> | SleepHandle;

export interface ReconnectDependencies {
	readonly createSession: WorkerSessionFactory;
	readonly sleep?: BackoffSleep;
	readonly logger?: WorkerLogger;
	/**
	 * Graceful-shutdown signal raced against every establishment-backoff
	 * sleep AND every in-flight establishment attempt (dial, handshake,
	 * register). Required (pass `undefined` explicitly when no shutdown
	 * signal exists) so no caller can silently keep the stall where a worker
	 * told to stop waits out the remaining backoff schedule or a hung dial.
	 */
	readonly signal: AbortSignal | undefined;
}

export interface WorkerLogger {
	info(message: string, fields?: Record<string, unknown>): void;
	warn(message: string, fields?: Record<string, unknown>): void;
	error(message: string, fields?: Record<string, unknown>): void;
}

/**
 * Classified failure for an exhausted reconnect budget — establishment
 * attempts inside {@link reconnectWithBackoff} or the worker loop's
 * cumulative cross-cycle drop budget. The last underlying failure is always
 * preserved as `cause` so callers never lose the original error detail.
 */
export class ReconnectExhaustedError extends Error {
	public constructor(message: string, options?: ErrorOptions) {
		super(message, options);
		this.name = "ReconnectExhaustedError";
	}
}

/**
 * Drop cause recorded when the server closes the worker stream cleanly
 * while a session factory is configured. A clean close is a retryable,
 * budgeted drop (it re-enters the reconnect cycle), so when a persistent
 * clean-close loop exhausts the drop budget the surfaced
 * {@link ReconnectExhaustedError} carries this error as its `cause` —
 * letting callers distinguish clean-close exhaustion from transport-failure
 * exhaustion with `instanceof`. Parity with the Rust worker's
 * `WorkerError::CleanCloseExhausted` and the Python worker's
 * `ServerClosedStreamError`.
 */
export class ServerClosedStreamError extends Error {
	public constructor(message: string) {
		super(message);
		this.name = "ServerClosedStreamError";
	}
}

/**
 * Connects, handshakes, and registers with bounded exponential backoff.
 * Deterministic PERMISSION_DENIED / UNAUTHENTICATED denials are rethrown
 * immediately instead of consuming further attempts; exhausting the budget
 * throws {@link ReconnectExhaustedError} with the last failure as `cause`.
 *
 * Shutdown wins promptly throughout the establishment cycle exactly as it
 * does during the worker loop's drop backoff: every backoff sleep AND every
 * in-flight establishment attempt (dial, handshake, register) is raced
 * against `dependencies.signal`, and no further dial is attempted once it
 * aborts — parity with the Rust worker, which selects shutdown around the
 * entire establishment in `run_with_connector_until`. Resolves `undefined`
 * when shutdown ended the establishment cycle so the caller returns cleanly;
 * a failed attempt's partially-established session is always closed before
 * the backoff begins, and an attempt abandoned to shutdown closes its
 * session in the background when the attempt eventually settles.
 */
export async function reconnectWithBackoff(
	config: WorkerConfig,
	activityTypes: readonly string[],
	dependencies: ReconnectDependencies,
): Promise<WorkerSession | undefined> {
	const reconnect = requireReconnectConfig(config.reconnect);
	let attempt = 1;
	let lastError: unknown;

	while (attempt <= reconnect.maxAttempts) {
		if (dependencies.signal?.aborted === true) {
			dependencies.logger?.info(
				"worker shutdown requested during reconnect; not dialling",
				{ attempt },
			);
			return undefined;
		}
		dependencies.logger?.info("worker reconnect attempt", { attempt });
		const establishment = establishSession(config, activityTypes, dependencies);
		const outcome = await raceEstablishmentAgainstAbort(
			establishment,
			dependencies.signal,
		);
		if (outcome.kind === "aborted") {
			dependencies.logger?.info(
				"worker shutdown requested during an in-flight dial; abandoning the attempt",
				{ attempt },
			);
			closeAbandonedEstablishment(establishment, dependencies.logger);
			return undefined;
		}
		if (outcome.kind === "established") {
			dependencies.logger?.info("worker reconnect succeeded", { attempt });
			return outcome.session;
		}
		const error = outcome.error;
		lastError = error;
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
		await sleepUnlessAborted(
			dependencies.sleep ?? defaultSleep,
			delayForAttempt(reconnect, attempt),
			dependencies.signal,
		);
		attempt += 1;
	}

	throw new ReconnectExhaustedError("worker reconnect attempts exhausted", {
		cause: lastError,
	});
}

/**
 * One full establishment attempt: dial, handshake, register. The
 * partially-established session is closed on the attempt's OWN failure path
 * — not by the caller — so the close still happens when the attempt loses
 * the shutdown race and finishes in the background.
 */
async function establishSession(
	config: WorkerConfig,
	activityTypes: readonly string[],
	dependencies: ReconnectDependencies,
): Promise<WorkerSession> {
	let session: WorkerSession | undefined;
	try {
		session = await dependencies.createSession(config);
		await session.handshake(config);
		await session.register(activityTypes);
		return session;
	} catch (error) {
		await closeFailedSession(session, dependencies.logger);
		throw error;
	}
}

export type EstablishmentOutcome =
	| { readonly kind: "established"; readonly session: WorkerSession }
	| { readonly kind: "failed"; readonly error: unknown }
	| { readonly kind: "aborted" };

/**
 * Races a session-establishment promise — a full dial/handshake/register
 * attempt inside {@link reconnectWithBackoff}, or the worker's initial dial
 * — against the shutdown signal so a SIGTERM during a hung connect returns
 * promptly instead of waiting out the transport's own connect behaviour.
 * The attempt promise is converted to a settled outcome before the race, so
 * a rejection that loses to the abort is always consumed — never an
 * unhandled rejection. The abort listener is removed on every exit so
 * repeated attempts never accumulate listeners.
 */
export async function raceEstablishmentAgainstAbort(
	establishment: Promise<WorkerSession>,
	signal: AbortSignal | undefined,
): Promise<EstablishmentOutcome> {
	const settled: Promise<EstablishmentOutcome> = establishment.then(
		(session): EstablishmentOutcome => ({ kind: "established", session }),
		(error: unknown): EstablishmentOutcome => ({ kind: "failed", error }),
	);
	if (signal === undefined) {
		return settled;
	}
	let unsubscribe = (): void => undefined;
	const aborted = new Promise<EstablishmentOutcome>((resolve) => {
		const onAbort = (): void => {
			resolve({ kind: "aborted" });
		};
		if (signal.aborted) {
			onAbort();
			return;
		}
		signal.addEventListener("abort", onAbort, { once: true });
		unsubscribe = (): void => {
			signal.removeEventListener("abort", onAbort);
		};
	});
	try {
		return await Promise.race([settled, aborted]);
	} finally {
		unsubscribe();
	}
}

/**
 * Attaches the close continuation to an establishment attempt abandoned to
 * shutdown: the losing attempt keeps running in the background, and the
 * session it eventually resolves must not leak its transport. A late
 * failure needs no close here (a reconnect attempt closed its partial
 * session inside {@link establishSession}; a bare initial dial never
 * exposed one) and is logged — acceptable only because the worker is
 * exiting — never an unhandled rejection.
 */
export function closeAbandonedEstablishment(
	establishment: Promise<WorkerSession>,
	logger: WorkerLogger | undefined,
): void {
	void establishment.then(
		async (session) => {
			logger?.info(
				"worker session established after shutdown; closing the abandoned session",
			);
			await closeFailedSession(session, logger);
		},
		(error: unknown) => {
			logger?.warn(
				"worker session establishment abandoned at shutdown failed",
				{
					message: error instanceof Error ? error.message : String(error),
				},
			);
		},
	);
}

/**
 * Runs the injectable backoff sleep but resolves immediately when the
 * shutdown signal aborts, so a worker told to stop during a long backoff —
 * an establishment retry or a drop recovery — never stalls for the
 * remainder of the delay (a SIGTERM-to-SIGKILL window in orchestrated
 * deployments). The caller re-checks the signal after this resolves; the
 * abort listener is always removed so repeated backoffs never accumulate
 * listeners, and a cancellable sleep ({@link SleepHandle}) is always
 * cancelled on the way out so an abort that wins the race disarms the timer
 * the sleep started — a lost race must never leave a timer holding the
 * event loop open for the remainder of the backoff.
 */
export async function sleepUnlessAborted(
	sleep: BackoffSleep,
	delayMs: number,
	signal: AbortSignal | undefined,
): Promise<void> {
	if (signal === undefined) {
		await toSleepHandle(sleep(delayMs)).done;
		return;
	}
	if (signal.aborted) {
		return;
	}
	const handle = toSleepHandle(sleep(delayMs));
	let unsubscribe = (): void => undefined;
	const aborted = new Promise<void>((resolve) => {
		const onAbort = (): void => {
			resolve();
		};
		signal.addEventListener("abort", onAbort, { once: true });
		unsubscribe = (): void => {
			signal.removeEventListener("abort", onAbort);
		};
	});
	try {
		await Promise.race([handle.done, aborted]);
	} finally {
		unsubscribe();
		// Disarm the timer regardless of who won: cancelling a completed
		// sleep is a documented no-op, and cancelling after an abort win is
		// the whole point.
		handle.cancel();
	}
}

/**
 * Normalises the two shapes the {@link BackoffSleep} seam accepts. A
 * promise-shaped result (anything `then`-able — every injected test sleep)
 * carries no cancellation, so its handle's `cancel` is a no-op; a
 * {@link SleepHandle} passes through unchanged.
 */
function toSleepHandle(started: Promise<void> | SleepHandle): SleepHandle {
	if (isPromiseShaped(started)) {
		return { done: started, cancel: () => undefined };
	}
	return started;
}

function isPromiseShaped(
	started: Promise<void> | SleepHandle,
): started is Promise<void> {
	return typeof (started as { readonly then?: unknown }).then === "function";
}

/**
 * Closes a session whose handshake, registration, or receive stream failed
 * so its transport resources are released before the next attempt (or
 * before the failure is surfaced on the fail-fast path). A close failure is
 * secondary: it is logged and never allowed to mask the original error,
 * matching the Rust and Python workers' semantics.
 */
export async function closeFailedSession(
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
			workflowId: report.workflowId,
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

/**
 * Bounded exponential backoff delay for the given one-based attempt:
 * `initialDelayMs` doubled once per prior attempt, capped at `maxDelayMs`.
 * Shared by establishment retries inside {@link reconnectWithBackoff} and
 * the worker loop's cross-cycle drop recovery, so both follow the
 * operator-supplied schedule (parity with the Python worker's
 * `ReconnectBackoff.delay_for_attempt` and the Rust worker's
 * `ReconnectBackoff::delay_for_attempt`) — no separate invented default.
 */
export function delayForAttempt(
	reconnect: ReconnectConfig,
	attempt: number,
): number {
	if (!Number.isInteger(attempt) || attempt <= 0) {
		throw new Error("reconnect attempt must be a positive integer");
	}
	return Math.min(
		reconnect.initialDelayMs * 2 ** (attempt - 1),
		reconnect.maxDelayMs,
	);
}

/**
 * Production backoff sleep: a real `setTimeout` exposed as a cancellable
 * {@link SleepHandle}. {@link sleepUnlessAborted} cancels the handle when
 * the shutdown abort wins the race, clearing the armed timer so a worker
 * that exits by event-loop drain is never held open for up to one max
 * backoff after SIGTERM (the establishment-backoff regime is exactly where
 * the longest sleeps live). Cancelling after completion is a no-op; the
 * timer is deliberately NOT `unref`ed, so an uncancelled legitimate sleep
 * keeps its normal drain semantics.
 */
export function defaultSleep(delayMs: number): SleepHandle {
	let timer: ReturnType<typeof setTimeout> | undefined;
	const done = new Promise<void>((resolve) => {
		timer = setTimeout(() => {
			timer = undefined;
			resolve();
		}, delayMs);
	});
	return {
		done,
		cancel(): void {
			if (timer !== undefined) {
				clearTimeout(timer);
				timer = undefined;
			}
		},
	};
}
