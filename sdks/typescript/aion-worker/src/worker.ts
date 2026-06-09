import {
	ActivityRegistry,
	type ActivityDefinition,
	type ActivityRegistryOptions,
} from "./activity.js";
import { runWorkerLoop } from "./loop.js";
import type { WorkerLogger } from "./reconnect.js";
import {
	connectGrpcWorkerSession,
	type GrpcClientFactory,
	type WorkerConfig,
	type WorkerSession,
	type WorkerSessionFactory,
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

	public async run(options: WorkerRunOptions = {}): Promise<void> {
		const session = await this.connect();
		const dispatcher = this.dispatcher(session);
		const signal = options.signal ?? this.options.signal;
		let removeAbortListener: (() => void) | undefined;
		if (signal !== undefined) {
			removeAbortListener = this.onAbort(signal, session, dispatcher);
		}
		try {
			await runWorkerLoop({
				config: this.config,
				session,
				dispatcher,
				logger: this.options.logger,
			});
		} finally {
			removeAbortListener?.();
		}
	}

	private async connect(): Promise<WorkerSession> {
		if (this.options.sessionFactory !== undefined) {
			return this.options.sessionFactory(this.config);
		}
		if (this.options.clientFactory !== undefined) {
			return connectGrpcWorkerSession(this.config, this.options.clientFactory);
		}
		throw new Error(
			"worker run requires a sessionFactory or clientFactory to connect",
		);
	}

	private dispatcher(session: WorkerSession): ActivityRegistry {
		const registryOptions: ActivityRegistryOptions = {
			logger: this.options.logger,
			heartbeatSender: session.sendHeartbeat.bind(session),
		};
		if (this.activities instanceof ActivityRegistry) {
			return this.activities.withSession(session, registryOptions);
		}
		return new ActivityRegistry(this.activities, registryOptions);
	}

	private onAbort(
		signal: AbortSignal,
		session: WorkerSession,
		dispatcher: ActivityRegistry,
	): () => void {
		const abort = (): void => {
			dispatcher.cancelAll?.();
			void session.close();
		};
		if (signal.aborted) {
			abort();
			return () => {};
		}
		signal.addEventListener("abort", abort, { once: true });
		return () => {
			signal.removeEventListener("abort", abort);
		};
	}
}
