import {
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
	WorkerSessionEvent,
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
		await receiveUntilClosed(session, running, loopOptions);
		if (options.sessionFactory === undefined) {
			await Promise.all(running);
			return;
		}
		await Promise.all(running);
		session = await reconnectWithBackoff(options.config, activityTypes, {
			createSession: options.sessionFactory,
			sleep: options.sleep,
			logger: options.logger,
		});
		await reReportUnacked(session, tracker, options.logger);
	}
}

async function receiveUntilClosed(
	session: WorkerSession,
	running: Set<Promise<void>>,
	options: RunWorkerLoopOptions,
): Promise<void> {
	const iterator = session.receiveTasks()[Symbol.asyncIterator]();
	for (;;) {
		await waitForSlot(running, options.config.maxConcurrency);
		const next = await readNext(iterator, options.logger);
		if (next === undefined) {
			return;
		}
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

async function readNext(
	iterator: AsyncIterator<WorkerSessionEvent>,
	logger: WorkerLogger | undefined,
): Promise<IteratorResult<WorkerSessionEvent> | undefined> {
	try {
		return await iterator.next();
	} catch (error) {
		logger?.warn("worker receive stream dropped", {
			message: error instanceof Error ? error.message : String(error),
		});
		return undefined;
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
