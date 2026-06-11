import { ActivityRegistry, type ActivityDefinition } from "./activity.js";
import { runWorkerLoop } from "./loop.js";
import {
	closeFailedSession,
	requireReconnectConfig,
	type WorkerLogger,
} from "./reconnect.js";
import {
	connectGrpcWorkerSession,
	type ActivityIdKey,
	type GrpcClientFactory,
	type Payload,
	type WorkerConfig,
	type WorkerSession,
	type WorkerSessionFactory,
	type WorkflowId,
} from "./session.js";

export interface WorkerOptions {
	readonly sessionFactory?: WorkerSessionFactory;
	readonly clientFactory?: GrpcClientFactory;
	readonly logger?: WorkerLogger;
	readonly signal?: AbortSignal;
}

export interface WorkerRunOptions {
	readonly signal?: AbortSignal;
}

export type WorkerActivities =
	| readonly ActivityDefinition[]
	| ActivityRegistry;

/**
 * Mutable holder for the worker's current session.
 *
 * `runWorkerLoop` replaces the target on every successful reconnect, so
 * heartbeats from activities always route to the live transport instead of
 * the session that originally delivered the task.
 *
 * Heartbeat semantics across reconnects: heartbeats are best-effort liveness
 * signals. A send that fails — including a send into the dead session during
 * the reconnect window — is logged as a warning and swallowed, never thrown
 * into the activity handler, so a transport gap can never fail an otherwise
 * healthy activity. This matches the Rust worker's handler-side semantics,
 * where handlers emit heartbeats through a channel decoupled from the
 * transport and a failed send surfaces in the loop machinery rather than in
 * the handler. A heartbeat lost this way is indistinguishable to the server
 * from the transport loss it is already tolerating; the activity's result is
 * what matters, and unacknowledged results are re-reported on the new
 * session by the loop's unacked-result tracker.
 */
class LiveSessionRouter {
	private session: WorkerSession;
	private readonly logger?: WorkerLogger;

	public constructor(initial: WorkerSession, logger?: WorkerLogger) {
		this.session = initial;
		this.logger = logger;
	}

	public replace(next: WorkerSession): void {
		this.session = next;
	}

	public current(): WorkerSession {
		return this.session;
	}

	public async sendHeartbeat(
		workflowId: WorkflowId,
		activityId: ActivityIdKey,
		progress?: Payload,
	): Promise<void> {
		try {
			await this.session.sendHeartbeat(workflowId, activityId, progress);
		} catch (error) {
			this.logger?.warn(
				"activity heartbeat failed; session may be reconnecting",
				{
					workflowId,
					activityId,
					message: error instanceof Error ? error.message : String(error),
				},
			);
		}
	}
}

export class Worker {
	private readonly config: WorkerConfig;
	private readonly activities: WorkerActivities;
	private readonly options: WorkerOptions;

	public constructor(
		config: WorkerConfig,
		activities: WorkerActivities,
		options: WorkerOptions = {},
	) {
		this.config = config;
		this.activities = activities;
		this.options = options;
	}

	/**
	 * Connects and serves activity tasks until a non-retryable server denial,
	 * reconnect-budget exhaustion, or an abort of the shutdown signal. A
	 * transient mid-run stream drop reconnects with the bounded backoff from
	 * `config.reconnect` (operator-supplied, validated up front — there are
	 * no SDK defaults) and re-reports unacknowledged results on the new
	 * session. Heartbeats always target the current session; see
	 * {@link LiveSessionRouter} for the across-reconnect semantics.
	 */
	public async run(options: WorkerRunOptions = {}): Promise<void> {
		const sessionFactory = this.sessionFactory();
		requireReconnectConfig(this.config.reconnect);
		const signal = options.signal ?? this.options.signal;
		if (signal?.aborted === true) {
			// A signal that is already aborted is a shutdown request predating
			// the run: return without connecting instead of dialling a session
			// the abort handler would immediately close (which would surface
			// the register write's write-after-end as a rejected run).
			this.options.logger?.info(
				"worker run aborted before connecting; not serving",
			);
			return;
		}
		const session = await sessionFactory(this.config);
		const liveSession = new LiveSessionRouter(session, this.options.logger);
		const dispatcher = this.dispatcher(liveSession);
		let removeAbortListener: (() => void) | undefined;
		if (signal !== undefined) {
			removeAbortListener = this.onAbort(signal, liveSession, dispatcher);
		}
		try {
			await runWorkerLoop({
				config: this.config,
				session,
				dispatcher,
				sessionFactory,
				signal,
				onSessionChange: (next) => {
					liveSession.replace(next);
				},
				logger: this.options.logger,
			});
		} finally {
			removeAbortListener?.();
		}
	}

	private sessionFactory(): WorkerSessionFactory {
		if (this.options.sessionFactory !== undefined) {
			return this.options.sessionFactory;
		}
		const clientFactory = this.options.clientFactory;
		if (clientFactory !== undefined) {
			return async (config) => connectGrpcWorkerSession(config, clientFactory);
		}
		throw new Error(
			"worker run requires a sessionFactory or clientFactory to connect",
		);
	}

	private dispatcher(liveSession: LiveSessionRouter): ActivityRegistry {
		if (this.activities instanceof ActivityRegistry) {
			return this.activities.withSession(liveSession, {
				logger: this.options.logger,
			});
		}
		return new ActivityRegistry(this.activities, {
			logger: this.options.logger,
			heartbeatSender: liveSession.sendHeartbeat.bind(liveSession),
		});
	}

	private onAbort(
		signal: AbortSignal,
		liveSession: LiveSessionRouter,
		dispatcher: ActivityRegistry,
	): () => void {
		const abort = (): void => {
			dispatcher.cancelAll();
			// Close the CURRENT session (the router is updated on reconnect);
			// close failures are logged by closeFailedSession, never dropped.
			void closeFailedSession(liveSession.current(), this.options.logger);
		};
		if (signal.aborted) {
			abort();
			return noop;
		}
		signal.addEventListener("abort", abort, { once: true });
		return () => {
			signal.removeEventListener("abort", abort);
		};
	}
}

function noop() {
	return undefined;
}
