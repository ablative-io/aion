import {
	closeFailedSession,
	grpcStatusCode,
	isRetryableSessionError,
	type PendingActivityReport,
	reconnectWithBackoff,
	reReportUnacked,
	UnackedResultTracker,
	type WorkerLogger,
} from "./reconnect.js";
import type {
	ActivityFailure,
	ActivityTask,
	Payload,
	WorkerConfig,
	WorkerSession,
	WorkerSessionFactory,
} from "./session.js";

export type DispatchOutcome =
	| { readonly kind: "completed"; readonly output: Payload }
	| { readonly kind: "failed"; readonly failure: ActivityFailure };

export interface ActivityDispatcher {
	activityTypes(): readonly string[];
	dispatch(task: ActivityTask): Promise<DispatchOutcome>;
	cancelAll?(): void;
}

export interface RunWorkerLoopOptions {
	readonly config: WorkerConfig;
	readonly session: WorkerSession;
	readonly dispatcher: ActivityDispatcher;
	readonly tracker?: UnackedResultTracker;
	readonly sessionFactory?: WorkerSessionFactory;
	readonly sleep?: (delayMs: number) => Promise<void>;
	readonly logger?: WorkerLogger;
	/**
	 * Graceful-shutdown signal. Once aborted, the loop stops reconnecting:
	 * a clean close or a handled retryable stream failure observed after the
	 * abort returns instead of dialling a replacement session the caller no
	 * longer wants, and a reconnect already in flight closes its fresh
	 * session and returns. Without a `sessionFactory` the signal is unused —
	 * no-factory mode already returns on clean close.
	 */
	readonly signal?: AbortSignal;
	/**
	 * Invoked with each replacement session the loop establishes after a
	 * reconnect, before any unacknowledged results are re-reported. Lets the
	 * caller repoint live-session consumers — activity heartbeats in
	 * particular — at the current transport instead of the dead one.
	 */
	readonly onSessionChange?: (session: WorkerSession) => void;
}

export async function runWorkerLoop(
	options: RunWorkerLoopOptions,
): Promise<void> {
	validateMaxConcurrency(options.config.maxConcurrency);
	let session = options.session;
	const activityTypes = options.dispatcher.activityTypes();
	await session.handshake(options.config);
	await session.register(activityTypes);

	const running = new Set<Promise<void>>();
	const tracker = options.tracker ?? new UnackedResultTracker();
	const loopOptions = { ...options, tracker };

	for (;;) {
		let streamFailure: StreamFailure | undefined;
		try {
			await receiveUntilClosed(session, running, loopOptions);
		} catch (error) {
			streamFailure = { error };
		}
		await Promise.all(running);
		if (streamFailure !== undefined) {
			await handleStreamFailure(streamFailure.error, session, options);
		}
		if (options.sessionFactory === undefined) {
			return;
		}
		if (shutdownRequested(options.signal)) {
			options.logger?.info("worker shutdown requested; not reconnecting");
			return;
		}
		session = await reconnectWithBackoff(options.config, activityTypes, {
			createSession: options.sessionFactory,
			sleep: options.sleep,
			logger: options.logger,
		});
		// Publish the replacement session before the post-reconnect abort
		// check: from here on an abort closes the new session through the
		// caller's live-session holder, so no shutdown window leaves it open.
		options.onSessionChange?.(session);
		if (shutdownRequested(options.signal)) {
			options.logger?.info(
				"worker shutdown requested during reconnect; closing replacement session",
			);
			await closeFailedSession(session, options.logger);
			return;
		}
		await reReportUnacked(session, tracker, options.logger);
	}
}

/**
 * Reads the abort flag through a function call so TypeScript's control-flow
 * narrowing never assumes the readonly `aborted` property is still false
 * after an `await` — abort listeners can fire at any suspension point.
 */
function shutdownRequested(signal: AbortSignal | undefined): boolean {
	return signal?.aborted === true;
}

/**
 * Wraps a thrown receive-stream error so a clean end-of-stream (the
 * iterator completing, or the session yielding its `closed` event) is never
 * confused with a stream failure — even one whose thrown value is itself
 * `undefined`.
 */
interface StreamFailure {
	readonly error: unknown;
}

/**
 * Classifies an error thrown by the receive stream. Deterministic server
 * denials (PERMISSION_DENIED / UNAUTHENTICATED) close the session and
 * propagate immediately — reconnecting can never fix them and would spin
 * forever because handshake/register succeed locally. Retryable failures
 * with no session factory also propagate (there is nothing to reconnect
 * with); otherwise the dead session is closed and control returns to the
 * bounded reconnect path. No stream error is ever logged-and-dropped.
 */
async function handleStreamFailure(
	error: unknown,
	session: WorkerSession,
	options: RunWorkerLoopOptions,
): Promise<void> {
	const message = error instanceof Error ? error.message : String(error);
	if (!isRetryableSessionError(error)) {
		options.logger?.error("worker stream denied by server; not reconnecting", {
			code: grpcStatusCode(error),
			message,
		});
		await closeFailedSession(session, options.logger);
		throw error;
	}
	if (options.sessionFactory === undefined) {
		options.logger?.error(
			"worker receive stream failed with no session factory to reconnect",
			{ message },
		);
		await closeFailedSession(session, options.logger);
		throw error;
	}
	options.logger?.warn("worker receive stream failed; reconnecting", {
		message,
	});
	await closeFailedSession(session, options.logger);
}

async function receiveUntilClosed(
	session: WorkerSession,
	running: Set<Promise<void>>,
	options: RunWorkerLoopOptions,
): Promise<void> {
	const iterator = session.receiveTasks()[Symbol.asyncIterator]();
	for (;;) {
		await waitForSlot(running, options.config.maxConcurrency);
		// A clean close is the iterator completing (`done: true`) or the
		// session yielding its `closed` event. A rejection from `next()` is a
		// stream error and must propagate to the caller for retryable /
		// fail-fast classification — never be converted into a silent close.
		const next = await iterator.next();
		if (next.done === true) {
			return;
		}
		const event = next.value;
		if (event.kind === "closed") {
			return;
		}
		options.logger?.info("worker received activity task", {
			workflowId: event.task.workflowId,
			activityId: event.task.activityId,
			activityType: event.task.activityType,
			attempt: event.task.attempt,
		});
		const taskPromise = dispatchAndReport(event.task, session, options).finally(
			() => {
				running.delete(taskPromise);
			},
		);
		running.add(taskPromise);
	}
}

async function dispatchAndReport(
	task: ActivityTask,
	session: WorkerSession,
	options: RunWorkerLoopOptions,
): Promise<void> {
	const outcome = await dispatchWithClassification(task, options);
	if (outcome.kind === "completed") {
		const report: PendingActivityReport = {
			kind: "completed",
			workflowId: task.workflowId,
			activityId: task.activityId,
			result: outcome.output,
		};
		options.tracker?.record(report);
		options.logger?.info("worker reporting completed activity", {
			activityId: task.activityId,
		});
		await reportSafely(
			() =>
				session.reportResult(task.workflowId, task.activityId, outcome.output),
			task,
			options,
		);
	} else {
		const report: PendingActivityReport = {
			kind: "failed",
			workflowId: task.workflowId,
			activityId: task.activityId,
			failure: outcome.failure,
		};
		options.tracker?.record(report);
		options.logger?.info("worker reporting failed activity", {
			activityId: task.activityId,
			retryable: outcome.failure.retryable,
		});
		await reportSafely(
			() =>
				session.reportFailure(
					task.workflowId,
					task.activityId,
					outcome.failure,
				),
			task,
			options,
		);
	}
}

async function dispatchWithClassification(
	task: ActivityTask,
	options: RunWorkerLoopOptions,
): Promise<DispatchOutcome> {
	try {
		return await options.dispatcher.dispatch(task);
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error);
		options.logger?.warn("worker dispatcher threw unclassified error", {
			activityId: task.activityId,
			activityType: task.activityType,
			retryable: true,
			message,
		});
		return {
			kind: "failed",
			failure: {
				retryable: true,
				message,
			},
		};
	}
}

async function reportSafely(
	report: () => Promise<void>,
	task: ActivityTask,
	options: RunWorkerLoopOptions,
): Promise<void> {
	try {
		await report();
	} catch (error) {
		options.logger?.warn(
			"worker report failed; result remains unacknowledged",
			{
				activityId: task.activityId,
				activityType: task.activityType,
				message: error instanceof Error ? error.message : String(error),
			},
		);
	}
}

async function waitForSlot(
	running: Set<Promise<void>>,
	maxConcurrency: number,
): Promise<void> {
	while (running.size >= maxConcurrency) {
		await Promise.race(running);
	}
}

function validateMaxConcurrency(maxConcurrency: number): void {
	if (!Number.isInteger(maxConcurrency) || maxConcurrency <= 0) {
		throw new Error("worker maxConcurrency must be a positive integer");
	}
}
