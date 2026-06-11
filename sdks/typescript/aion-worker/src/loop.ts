import {
	closeFailedSession,
	defaultSleep,
	delayForAttempt,
	grpcStatusCode,
	isRetryableSessionError,
	type PendingActivityReport,
	ReconnectExhaustedError,
	reconnectWithBackoff,
	requireReconnectConfig,
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
	 * session and returns. A signal aborted before (or during) the initial
	 * handshake/register returns cleanly without serving — the abort handler
	 * closes the session, so the registration write failing is shutdown, not
	 * an error. Without a `sessionFactory` the loop never reconnects, so the
	 * signal only gates that initial registration window.
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
	const reconnect = requireReconnectConfig(options.config.reconnect);
	let session = options.session;
	const activityTypes = options.dispatcher.activityTypes();
	if (shutdownRequested(options.signal)) {
		options.logger?.info(
			"worker shutdown requested before registration; not serving",
		);
		await closeFailedSession(session, options.logger);
		return;
	}
	try {
		await session.handshake(options.config);
		await session.register(activityTypes);
	} catch (error) {
		await closeFailedSession(session, options.logger);
		if (!isRetryableSessionError(error)) {
			// A deterministic server denial (PERMISSION_DENIED /
			// UNAUTHENTICATED) outranks a racing abort: downgrading it to a
			// graceful return would hand the supervisor a clean exit and only
			// surface the denial after the restart. Classify first — an
			// abort-induced local close error ("write after end") is not
			// gRPC-shaped, so it still reaches the graceful path below.
			options.logger?.error(
				"worker registration denied by server; not serving",
				{ code: grpcStatusCode(error), message: describeError(error) },
			);
			throw error;
		}
		if (shutdownRequested(options.signal)) {
			// An abort raced the initial registration: the abort handler closes
			// the session, so the in-flight register write fails (write after
			// end). A graceful shutdown is not an error — return cleanly
			// instead of surfacing the write failure as a rejected run.
			options.logger?.info(
				"worker shutdown requested during registration; not serving",
				{ message: describeError(error) },
			);
			return;
		}
		throw error;
	}

	const running = new Set<Promise<void>>();
	const tracker = options.tracker ?? new UnackedResultTracker();
	const loopOptions = { ...options, tracker };
	const sleep = options.sleep ?? defaultSleep;
	// Cumulative cross-cycle drop budget: every dropped session in this run —
	// a retryable stream failure, a clean close re-entering the cycle, or a
	// failed result replay — consumes one unit of `reconnect.maxAttempts`,
	// and the budget never resets. This is parity with the Python worker
	// (`connect_register_replay_and_serve`) and the Rust worker
	// (`run_with_connector_until`): without it, a server that accepts the
	// stream and immediately drops (or cleanly closes) it would spin the
	// loop forever at full CPU with no delay and no surfaced error.
	let droppedAttempts = 0;

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
			// Clean close with nothing to reconnect with: stream failures have
			// already propagated inside handleStreamFailure.
			return;
		}
		if (shutdownRequested(options.signal)) {
			options.logger?.info("worker shutdown requested; not reconnecting");
			return;
		}
		let dropError: unknown;
		if (streamFailure !== undefined) {
			dropError = streamFailure.error;
		} else {
			// A clean close with a factory present re-enters the reconnect
			// cycle, so an endlessly clean-closing server is the same hazard
			// as a flapping stream: it must consume the same drop budget. The
			// replaced session is closed (idempotently) so its write side is
			// released before the replacement is dialled.
			dropError = new Error(
				"worker receive stream closed cleanly while a session factory was configured",
			);
			options.logger?.warn(
				"worker stream closed cleanly; treating as a session drop",
			);
			await closeFailedSession(session, options.logger);
		}

		// Recovery: consume the drop budget, back off on the configured
		// schedule, reconnect, and replay unacknowledged results. A retryable
		// replay failure consumes another drop and re-enters; a non-retryable
		// one propagates; a shutdown observed anywhere returns cleanly.
		for (;;) {
			droppedAttempts += 1;
			if (droppedAttempts >= reconnect.maxAttempts) {
				options.logger?.error(
					"worker session drop budget exhausted; not reconnecting",
					{ droppedAttempts, message: describeError(dropError) },
				);
				throw new ReconnectExhaustedError(
					`worker session drop budget exhausted after ${String(droppedAttempts)} dropped sessions: ${describeError(dropError)}`,
					{ cause: dropError },
				);
			}
			const delayMs = delayForAttempt(reconnect, droppedAttempts);
			options.logger?.warn(
				"worker session dropped; reconnecting after backoff",
				{ droppedAttempts, delayMs, message: describeError(dropError) },
			);
			await sleep(delayMs);
			if (shutdownRequested(options.signal)) {
				options.logger?.info(
					"worker shutdown requested during drop backoff; not reconnecting",
				);
				return;
			}
			session = await reconnectWithBackoff(options.config, activityTypes, {
				createSession: options.sessionFactory,
				sleep: options.sleep,
				logger: options.logger,
			});
			// Publish the replacement session before the post-reconnect abort
			// check: from here on an abort closes the new session through the
			// caller's live-session holder, so no shutdown window leaves it
			// open.
			options.onSessionChange?.(session);
			if (shutdownRequested(options.signal)) {
				options.logger?.info(
					"worker shutdown requested during reconnect; closing replacement session",
				);
				await closeFailedSession(session, options.logger);
				return;
			}
			try {
				await reReportUnacked(session, tracker, options.logger);
				break;
			} catch (error) {
				await closeFailedSession(session, options.logger);
				if (!isRetryableSessionError(error)) {
					// A deterministic server denial outranks a racing abort —
					// see the registration catch above for the rationale. An
					// abort-induced "write after end" is not gRPC-shaped and
					// still takes the graceful return below.
					options.logger?.error(
						"worker result replay denied by server; not reconnecting",
						{ code: grpcStatusCode(error), message: describeError(error) },
					);
					throw error;
				}
				if (shutdownRequested(options.signal)) {
					// An abort landed between the post-reconnect shutdown check
					// and the replay write, closing the just-published session
					// out from under it. Graceful shutdown must not turn into a
					// rejected run: the unacked results stay tracked, and the
					// caller asked us to stop.
					options.logger?.info(
						"worker shutdown requested during result replay; not reconnecting",
						{ message: describeError(error) },
					);
					return;
				}
				options.logger?.warn(
					"worker result replay failed; counting against drop budget",
					{ message: describeError(error) },
				);
				dropError = error;
			}
		}
	}
}

function describeError(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
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
