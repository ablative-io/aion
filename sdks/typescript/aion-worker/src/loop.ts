import {
	type BackoffSleep,
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
	ServerClosedStreamError,
	sleepUnlessAborted,
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
	readonly sleep?: BackoffSleep;
	readonly logger?: WorkerLogger;
	/**
	 * Graceful-shutdown signal. Once aborted, the loop stops reconnecting:
	 * a clean close or a handled retryable stream failure observed after the
	 * abort returns instead of dialling a replacement session the caller no
	 * longer wants, and a reconnect already in flight closes its fresh
	 * session and returns. The signal wins promptly during BOTH backoff
	 * phases — the drop backoff and the establishment-retry backoffs inside
	 * `reconnectWithBackoff` each race their sleep against the abort — AND
	 * during an in-flight establishment attempt: `reconnectWithBackoff`
	 * races the whole dial/handshake/register chain against the abort, so a
	 * stopping worker never waits out a backoff schedule or a hung dial. A
	 * signal aborted before (or during) the initial
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
	/**
	 * Monotonic millisecond clock used for session-health accounting (how
	 * long an established session stayed connected — measured from
	 * successful registration to the moment its receive stream ended, never
	 * to the end of the post-drop drain — versus `reconnect.maxDelayMs` when
	 * deciding drop-budget resets). Defaults to `performance.now`;
	 * injectable so tests can script time.
	 */
	readonly now?: () => number;
}

/** Per-session liveness counter used for drop-budget reset accounting. */
interface SessionHealth {
	tasksServed: number;
	/**
	 * Latched when a drain frame is observed on this session: the eventual
	 * stream end — clean OR abrupt — is then drain-class (the server
	 * announced it was going away), so the drop consumes no budget.
	 */
	drainReceived: boolean;
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
	const now = options.now ?? defaultNow;
	// Cumulative cross-cycle drop budget: every dropped session in this run —
	// a retryable stream failure, a clean close re-entering the cycle, or a
	// failed result replay — consumes one unit of `reconnect.maxAttempts`.
	// The budget resets to zero once an established session proves healthy:
	// it served at least one task, or it survived longer than
	// `reconnect.maxDelayMs` (the policy's own longest pause, so a session
	// outliving it is demonstrably past the flapping regime; a served task
	// proves end-to-end health). This is parity with the Python worker
	// (`connect_register_replay_and_serve`) and the Rust worker
	// (`run_with_connector_until`): a genuinely flapping server — no session
	// ever serves a task or outlives the cap — still exhausts the budget
	// after exactly `maxAttempts` drops, so it can never spin the loop
	// forever at full CPU, while routine server deploys cost only transient
	// budget that heals.
	let droppedAttempts = 0;
	// Session-health accounting starts at the just-registered initial session.
	let sessionStartedAt = now();
	// Captured the moment the receive stream ends or drops — before in-flight
	// handlers are drained — so the drop-budget reset decision measures
	// connected time, never drain time (a long-running handler must not let a
	// short-lived session masquerade as one that outlived the max backoff).
	let sessionEndedAt = sessionStartedAt;
	let sessionHealth: SessionHealth = { tasksServed: 0, drainReceived: false };

	for (;;) {
		let streamFailure: StreamFailure | undefined;
		try {
			await receiveUntilClosed(session, running, loopOptions, sessionHealth);
		} catch (error) {
			streamFailure = { error };
		}
		sessionEndedAt = now();
		await Promise.all(running);
		if (streamFailure !== undefined && !sessionHealth.drainReceived) {
			await handleStreamFailure(streamFailure.error, session, options);
		}
		if (options.sessionFactory === undefined) {
			// Clean close with nothing to reconnect with: stream failures have
			// already propagated inside handleStreamFailure (a drain-latched
			// failure with no factory still has nothing to reconnect with, so
			// it is surfaced too). The session is closed before returning so
			// no exit path leaks its transport.
			if (streamFailure !== undefined && sessionHealth.drainReceived) {
				await handleStreamFailure(streamFailure.error, session, options);
			}
			await closeFailedSession(session, options.logger);
			return;
		}
		if (shutdownRequested(options.signal)) {
			options.logger?.info("worker shutdown requested; not reconnecting");
			if (streamFailure === undefined || sessionHealth.drainReceived) {
				// Failure paths already closed the session inside
				// handleStreamFailure; every other end must close it here so
				// no exit path leaks the transport.
				await closeFailedSession(session, options.logger);
			}
			return;
		}
		// Drain latch: once the drain frame was observed, this session's end —
		// clean or abrupt — is drain-class and consumes no drop budget.
		let drainClass = sessionHealth.drainReceived;
		let dropError: unknown;
		if (streamFailure !== undefined) {
			dropError = streamFailure.error;
			if (drainClass) {
				// A denial would have propagated above only when non-drained;
				// after a drain the announced classification wins for
				// retryable transport ends, but a deterministic denial still
				// outranks it.
				if (!isRetryableSessionError(streamFailure.error)) {
					await handleStreamFailure(streamFailure.error, session, options);
				}
				options.logger?.warn(
					"worker session error after server drain; classified as drain drop",
					{ message: describeError(streamFailure.error) },
				);
				await closeFailedSession(session, options.logger);
			}
		} else if (drainClass) {
			options.logger?.info(
				"worker session drained by server; reconnecting after initial backoff",
			);
			await closeFailedSession(session, options.logger);
			dropError = undefined;
		} else {
			// An unannounced clean close with a factory present re-enters the
			// reconnect cycle, so an endlessly clean-closing server is the
			// same hazard as a flapping stream: it must consume the same drop
			// budget. The replaced session is closed (idempotently) so its
			// write side is released before the replacement is dialled.
			dropError = new ServerClosedStreamError(
				"worker receive stream closed cleanly while a session factory was configured",
			);
			options.logger?.warn(
				"worker stream closed cleanly; treating as a session drop",
			);
			await closeFailedSession(session, options.logger);
		}

		// Recovery: consume the drop budget (drain-class drops are exempt),
		// back off on the configured schedule, reconnect, and replay
		// unacknowledged results. A retryable replay failure consumes another
		// drop and re-enters; a non-retryable one propagates; a shutdown
		// observed anywhere applies the cross-SDK outcome rule (pending
		// drain/clean-close drop returns cleanly, pending error-class drop
		// surfaces its error).
		for (;;) {
			// The dropped session (the served one on the first iteration, the
			// replay-failed replacement on re-entries) resets the budget when
			// it proved healthy before dropping. Connected time runs from
			// registration to the recorded stream end (or replay failure) —
			// post-drop draining never extends it.
			if (
				sessionHealth.tasksServed > 0 ||
				sessionEndedAt - sessionStartedAt > reconnect.maxDelayMs
			) {
				if (droppedAttempts > 0) {
					options.logger?.info(
						"worker session proved healthy; drop budget reset",
						{ droppedAttempts, tasksServed: sessionHealth.tasksServed },
					);
				}
				droppedAttempts = 0;
			}
			let delayMs: number;
			if (drainClass) {
				// An announced drain consumes no drop budget: the server told
				// the worker it was going away, so the drop is expected
				// operator behaviour, not flapping.
				delayMs = reconnect.initialDelayMs;
			} else {
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
				delayMs = delayForAttempt(reconnect, droppedAttempts);
			}
			options.logger?.warn(
				"worker session dropped; reconnecting after backoff",
				{ droppedAttempts, delayMs, message: describeError(dropError) },
			);
			await sleepUnlessAborted(sleep, delayMs, options.signal);
			if (shutdownRequested(options.signal)) {
				// Cross-SDK shutdown-outcome rule: a pending drain-class or
				// clean-close drop ends the run cleanly; a pending error-class
				// drop surfaces its error.
				if (!drainClass && !(dropError instanceof ServerClosedStreamError)) {
					options.logger?.info(
						"worker shutdown requested during error-drop backoff; surfacing pending drop",
						{ message: describeError(dropError) },
					);
					throw dropError;
				}
				options.logger?.info(
					"worker shutdown requested during drop backoff; not reconnecting",
				);
				return;
			}
			const replacement = await reconnectWithBackoff(
				options.config,
				activityTypes,
				{
					createSession: options.sessionFactory,
					sleep: options.sleep,
					logger: options.logger,
					signal: options.signal,
				},
			);
			if (replacement === undefined) {
				// Shutdown fired during the establishment cycle (an
				// establishment-backoff sleep or an in-flight dial): return
				// cleanly, mirroring the drop-backoff shutdown path. No session
				// is open at this point — failed attempts close themselves and
				// an abandoned in-flight attempt closes its session in the
				// background inside reconnectWithBackoff.
				options.logger?.info(
					"worker shutdown requested during reconnect backoff; not reconnecting",
				);
				return;
			}
			session = replacement;
			// Health accounting restarts at the replacement session's
			// registration: its survival and served tasks are what may reset
			// the budget at its own eventual drop.
			sessionStartedAt = now();
			sessionEndedAt = sessionStartedAt;
			sessionHealth = { tasksServed: 0, drainReceived: false };
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
				// The replacement never entered the serve loop, so its drop
				// moment for health accounting is the replay failure itself.
				sessionEndedAt = now();
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
				// A replay failure is an unannounced drop: the latch belongs
				// to the drained session, never to its replacement.
				drainClass = false;
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
	health: SessionHealth,
): Promise<void> {
	const iterator = session.receiveTasks()[Symbol.asyncIterator]();
	// The RUNTIME owns liveness (#176): a session registered against a server
	// heartbeat window automatically heartbeats every in-flight activity at a
	// quarter-window cadence, so the server's heartbeat sweeper only ever
	// expires a genuinely dead/wedged process — never a healthy worker running
	// a legitimately long activity. Sessions without a window (fakes, tests)
	// never pump. Explicit `ActivityContext` heartbeats remain the way to
	// attach PROGRESS payloads. The pump lives exactly as long as this
	// receive loop, mirroring the Rust and Python runtimes.
	const inFlight = new Map<string, InFlightIdentity>();
	const pump = startLivenessPump(session, inFlight, options.logger);
	try {
		for (;;) {
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
			if (event.kind === "drained") {
				// Latch the drain classification and stop pulling: in-flight work
				// finishes and reports via the caller's `running` drain.
				options.logger?.info(
					"worker received server drain; finishing in-flight work before reconnect",
				);
				health.drainReceived = true;
				return;
			}
			if (event.kind === "resultAck") {
				// Acks are bookkeeping, not work: consumed without a concurrency
				// slot. An unknown ack is a logged no-op.
				acknowledgeResult(event.workflowId, event.activityId, options);
				continue;
			}
			// Only tasks occupy a concurrency slot; acks and drains were handled
			// above without one.
			await waitForSlot(running, options.config.maxConcurrency);
			options.logger?.info("worker received activity task", {
				workflowId: event.task.workflowId,
				activityId: event.task.activityId,
				activityType: event.task.activityType,
				attempt: event.task.attempt,
			});
			const inFlightKey = `${event.task.workflowId} ${event.task.activityId}`;
			inFlight.set(inFlightKey, {
				workflowId: event.task.workflowId,
				activityId: event.task.activityId,
			});
			const taskPromise = dispatchAndReport(
				event.task,
				session,
				options,
				health,
			).finally(() => {
				inFlight.delete(inFlightKey);
				running.delete(taskPromise);
			});
			running.add(taskPromise);
		}
	} finally {
		pump.stop();
	}
}

/** Identity of one in-flight activity, targeted by the liveness pump. */
interface InFlightIdentity {
	readonly workflowId: string;
	readonly activityId: string;
}

/** Handle for stopping the automatic liveness pump with its receive loop. */
interface LivenessPump {
	stop(): void;
}

/**
 * Starts the automatic liveness pump: every quarter of the server-assigned
 * heartbeat window it heartbeats each in-flight activity, with no progress
 * payload (explicit handler heartbeats own the progress channel). The
 * server's heartbeat sweeper expires any worker whose in-flight task goes a
 * full window without a heartbeat — that is dead/wedged-PROCESS detection,
 * and a healthy process running a legitimately long handler must never trip
 * it, so keeping tasks beating is the runtime's job, not each handler's
 * (#176). A wedged event loop stops this pump and is correctly expired. A
 * failed beat stops the pump: the receive stream is about to surface the
 * same transport failure and end the serve loop.
 */
function startLivenessPump(
	session: WorkerSession,
	inFlight: ReadonlyMap<string, InFlightIdentity>,
	logger: WorkerLogger | undefined,
): LivenessPump {
	const windowMs = session.heartbeatWindowMs?.();
	if (windowMs === undefined || windowMs <= 0) {
		return { stop: () => undefined };
	}
	const intervalMs = Math.max(windowMs / 4, 1);
	let stopped = false;
	let timer: ReturnType<typeof setTimeout> | undefined;
	const beat = async (): Promise<void> => {
		for (const entry of [...inFlight.values()]) {
			try {
				await session.sendHeartbeat(entry.workflowId, entry.activityId);
			} catch (error) {
				logger?.warn("automatic liveness heartbeat failed; pump stopped", {
					message: error instanceof Error ? error.message : String(error),
				});
				return;
			}
		}
		if (!stopped) {
			timer = setTimeout(run, intervalMs);
		}
	};
	const run = (): void => {
		void beat();
	};
	timer = setTimeout(run, intervalMs);
	return {
		stop: (): void => {
			stopped = true;
			if (timer !== undefined) {
				clearTimeout(timer);
			}
		},
	};
}

/**
 * Clears the acknowledged tracker entry; an unknown ack (already acked on a
 * previous session, or replaced by a re-record) is a logged no-op.
 */
function acknowledgeResult(
	workflowId: string,
	activityId: string,
	options: RunWorkerLoopOptions,
): void {
	if (options.tracker?.get(workflowId, activityId) !== undefined) {
		options.tracker.acknowledge(workflowId, activityId);
		options.logger?.info(
			"server acknowledged activity result; tracker entry cleared",
			{ workflowId, activityId },
		);
	} else {
		options.logger?.info("result ack for unknown tracker entry ignored", {
			workflowId,
			activityId,
		});
	}
}

async function dispatchAndReport(
	task: ActivityTask,
	session: WorkerSession,
	options: RunWorkerLoopOptions,
	health: SessionHealth,
): Promise<void> {
	const outcome = await dispatchWithClassification(task, options);
	let reported: boolean;
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
		reported = await reportSafely(
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
		reported = await reportSafely(
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
	if (reported) {
		// A received task whose outcome report went out is the end-to-end
		// health proof used for drop-budget resets.
		health.tasksServed += 1;
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

/**
 * Sends a report, swallowing (but logging) failures so an unacknowledged
 * result never kills the serve loop — the tracker re-reports it after the
 * next reconnect. Returns whether the send succeeded, which feeds the
 * session-health accounting used for drop-budget resets.
 */
async function reportSafely(
	report: () => Promise<void>,
	task: ActivityTask,
	options: RunWorkerLoopOptions,
): Promise<boolean> {
	try {
		await report();
		return true;
	} catch (error) {
		options.logger?.warn(
			"worker report failed; result remains unacknowledged",
			{
				activityId: task.activityId,
				activityType: task.activityType,
				message: error instanceof Error ? error.message : String(error),
			},
		);
		return false;
	}
}

/** Default monotonic clock for session-health accounting. */
function defaultNow(): number {
	return performance.now();
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
