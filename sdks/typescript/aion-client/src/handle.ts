import type {
  CancelOptions,
  Client,
  DescribeOptions,
  SignalOptions,
  SignalRawOptions,
  WorkflowDescription,
} from "./client.js";
import { UnavailableError } from "./errors.js";
import type { Payload } from "./payload.js";
import { eventStream, type WorkflowEvent } from "./stream.js";

export interface HandleSignalOptions<I> {
  readonly signalName: string;
  readonly payload: I;
  readonly runId?: string;
}

export interface HandleSignalRawOptions {
  readonly signalName: string;
  readonly payload: Payload;
  readonly runId?: string;
}

export interface HandleQueryOptions<I> {
  readonly queryName: string;
  readonly input?: I;
  readonly timeoutMs?: number;
  readonly runId?: string;
}

export interface HandleQueryRawOptions {
  readonly queryName: string;
  readonly input?: Payload;
  readonly timeoutMs?: number;
  readonly runId?: string;
}

export interface HandleCancelOptions {
  readonly reason?: string;
  readonly runId?: string;
}

export interface HandleDescribeOptions {
  readonly includeHistory?: boolean;
  readonly runId?: string;
}

export interface HandleSubscribeOptions {
  readonly runId?: string;
  readonly maxReconnects?: number;
}

export class WorkflowHandle {
  readonly client: Client;
  readonly workflowId: string;
  readonly runId?: string;
  readonly namespace: string;

  constructor(
    client: Client,
    workflowId: string,
    runId: string | undefined,
    namespace: string,
  ) {
    this.client = client;
    this.workflowId = workflowId;
    this.runId = runId;
    this.namespace = namespace;
  }

  async signal<I>(options: HandleSignalOptions<I>): Promise<void> {
    const request: SignalOptions<I> = {
      namespace: this.namespace,
      workflowId: this.workflowId,
      runId: this.targetRunId(options.runId),
      signalName: options.signalName,
      payload: options.payload,
    };
    await this.client.signal(request);
  }

  async signalRaw(options: HandleSignalRawOptions): Promise<void> {
    const request: SignalRawOptions = {
      namespace: this.namespace,
      workflowId: this.workflowId,
      runId: this.targetRunId(options.runId),
      signalName: options.signalName,
      payload: options.payload,
    };
    await this.client.signalRaw(request);
  }

  async query<I, R>(options: HandleQueryOptions<I>): Promise<R> {
    return this.client.query<I, R>({
      namespace: this.namespace,
      workflowId: this.workflowId,
      runId: this.targetRunId(options.runId),
      queryName: options.queryName,
      input: options.input,
      timeoutMs: options.timeoutMs,
    });
  }

  async queryRaw(options: HandleQueryRawOptions): Promise<Payload> {
    return this.client.queryRaw({
      namespace: this.namespace,
      workflowId: this.workflowId,
      runId: this.targetRunId(options.runId),
      queryName: options.queryName,
      input: options.input,
      timeoutMs: options.timeoutMs,
    });
  }

  async cancel(options: HandleCancelOptions = {}): Promise<void> {
    const request: CancelOptions = {
      namespace: this.namespace,
      workflowId: this.workflowId,
      runId: this.targetRunId(options.runId),
      reason: options.reason,
    };
    await this.client.cancel(request);
  }

  async describe(
    options: HandleDescribeOptions = {},
  ): Promise<WorkflowDescription> {
    const request: DescribeOptions = {
      namespace: this.namespace,
      workflowId: this.workflowId,
      runId: this.targetRunId(options.runId),
      includeHistory: options.includeHistory,
    };
    return this.client.describe(request);
  }

  subscribe(
    options: HandleSubscribeOptions = {},
  ): AsyncIterable<WorkflowEvent> {
    if (this.client.streamTransport === undefined) {
      throw new UnavailableError(
        "No subscribe transport configured for this client",
      );
    }
    return eventStream({
      transport: this.client.streamTransport,
      request: {
        namespace: this.namespace,
        workflowId: this.workflowId,
        runId: this.targetRunId(options.runId),
      },
      maxReconnects: options.maxReconnects,
    });
  }

  private targetRunId(runId: string | undefined): string | undefined {
    return runId ?? this.runId;
  }
}
