import { type AuthOptions, authHeaders } from "./auth.js";
import {
  AlreadyExistsError,
  InvalidArgumentError,
  QueryTimeoutError,
  mapHttpResponseError,
  mapTransportError,
  mapWireError,
} from "./errors.js";
import { WorkflowHandle } from "./handle.js";
import {
  type Payload,
  type WireEnvelope,
  fromPayload,
  toPayload,
} from "./payload.js";
import type { SubscribeTransport } from "./stream.js";
import { WebSocketSubscribeTransport } from "./ws.js";

export interface TlsOptions {
  readonly enabled?: boolean;
  readonly caCertificate?: string;
}

export interface ClientOptions {
  readonly endpoint: string;
  readonly auth?: AuthOptions;
  readonly tls?: TlsOptions;
  readonly namespace?: string;
  readonly transport?: WorkflowTransport;
  readonly streamTransport?: SubscribeTransport;
  readonly fetch?: typeof fetch;
}

export interface WorkflowRef {
  readonly workflowId: string;
  readonly runId?: string;
}

export interface StartOptions<I> {
  readonly namespace?: string;
  readonly workflowType: string;
  readonly input: I;
  readonly idempotencyKey?: string;
}

export interface StartRawOptions {
  readonly namespace?: string;
  readonly workflowType: string;
  readonly input: Payload;
  readonly idempotencyKey?: string;
}

export interface SignalOptions<I> extends WorkflowRef {
  readonly namespace?: string;
  readonly signalName: string;
  readonly payload: I;
}

export interface SignalRawOptions extends WorkflowRef {
  readonly namespace?: string;
  readonly signalName: string;
  readonly payload: Payload;
}

export interface QueryOptions<I> extends WorkflowRef {
  readonly namespace?: string;
  readonly queryName: string;
  readonly input?: I;
  readonly timeoutMs?: number;
}

export interface QueryRawOptions extends WorkflowRef {
  readonly namespace?: string;
  readonly queryName: string;
  readonly input?: Payload;
  readonly timeoutMs?: number;
}

export interface CancelOptions extends WorkflowRef {
  readonly namespace?: string;
  readonly reason?: string;
}

export interface ListOptions {
  readonly namespace?: string;
  readonly filter?: WireEnvelope;
}

export interface DescribeOptions extends WorkflowRef {
  readonly namespace?: string;
  readonly includeHistory?: boolean;
}

export interface WorkflowDescription {
  readonly summary?: WireEnvelope;
  readonly history: readonly WireEnvelope[];
}

export interface ListWorkflowsResult {
  readonly summaries: readonly WireEnvelope[];
}

export interface WorkflowTransport {
  start(
    request: ProtoStartWorkflowRequest,
  ): Promise<ProtoStartWorkflowResponse>;
  signal(request: ProtoSignalRequest): Promise<void>;
  query(request: ProtoQueryRequest): Promise<ProtoQueryResponse>;
  cancel(request: ProtoCancelRequest): Promise<void>;
  list(request: ProtoListWorkflowsRequest): Promise<ProtoListWorkflowsResponse>;
  describe(
    request: ProtoDescribeWorkflowRequest,
  ): Promise<ProtoDescribeWorkflowResponse>;
}

export interface ProtoWorkflowId {
  readonly uuid: string;
}

export interface ProtoRunId {
  readonly uuid: string;
}

export interface ProtoStartWorkflowRequest {
  readonly namespace: string;
  readonly workflow_type: string;
  readonly input: Payload;
}

export interface ProtoStartWorkflowResponse {
  readonly workflow_id?: ProtoWorkflowId;
  readonly run_id?: ProtoRunId;
}

export interface ProtoSignalRequest {
  readonly namespace: string;
  readonly workflow_id: ProtoWorkflowId;
  readonly run_id?: ProtoRunId;
  readonly signal_name: string;
  readonly payload: Payload;
}

export interface ProtoQueryRequest {
  readonly namespace: string;
  readonly workflow_id: ProtoWorkflowId;
  readonly run_id?: ProtoRunId;
  readonly query_name: string;
}

export interface ProtoQueryResponse {
  readonly result?: Payload;
  readonly error?: unknown;
}

export interface ProtoCancelRequest {
  readonly namespace: string;
  readonly workflow_id: ProtoWorkflowId;
  readonly run_id?: ProtoRunId;
  readonly reason?: string;
}

export interface ProtoListWorkflowsRequest {
  readonly namespace: string;
  readonly filter?: WireEnvelope;
}

export interface ProtoListWorkflowsResponse {
  readonly summaries?: readonly WireEnvelope[];
}

export interface ProtoDescribeWorkflowRequest {
  readonly namespace: string;
  readonly workflow_id: ProtoWorkflowId;
  readonly run_id?: ProtoRunId;
  readonly include_history: boolean;
}

export interface ProtoDescribeWorkflowResponse {
  readonly summary?: WireEnvelope;
  readonly history?: readonly WireEnvelope[];
}

export async function connect(options: ClientOptions): Promise<Client> {
  const client = new Client(options);
  return client;
}

export class Client {
  private readonly endpoint: string;
  private readonly namespace: string;
  private readonly transport: WorkflowTransport;
  readonly streamTransport: SubscribeTransport;
  /**
   * SDK-boundary start idempotency (the contract's hard case): the same key
   * retried with an identical request returns the original handle;
   * conflicting reuse throws {@link AlreadyExistsError}. Keyed by
   * idempotency key, fingerprinted over namespace, workflow type, and the
   * encoded input payload.
   */
  private readonly idempotentStarts = new Map<
    string,
    { readonly fingerprint: string; readonly handle: WorkflowHandle }
  >();

  constructor(options: ClientOptions) {
    this.endpoint = normalizeEndpoint(options.endpoint, options.tls);
    this.namespace = options.namespace ?? "default";
    this.transport =
      options.transport ??
      new HttpWorkflowTransport(
        this.endpoint,
        options.auth,
        options.fetch ?? fetch,
      );
    // An injected transport wins; otherwise subscriptions ride the built-in
    // WebSocket transport against the same listener as the HTTP endpoint.
    this.streamTransport =
      options.streamTransport ??
      new WebSocketSubscribeTransport({
        endpoint: this.endpoint,
        auth: options.auth,
      });
  }

  async start<I>(options: StartOptions<I>): Promise<WorkflowHandle> {
    return this.startRaw({
      ...options,
      input: toPayload(options.input),
    });
  }

  async startRaw(options: StartRawOptions): Promise<WorkflowHandle> {
    const namespace = this.resolveNamespace(options.namespace);
    const fingerprint =
      options.idempotencyKey === undefined
        ? undefined
        : JSON.stringify([
            namespace,
            options.workflowType,
            options.input.content_type,
            options.input.bytes,
          ]);
    if (options.idempotencyKey !== undefined && fingerprint !== undefined) {
      const cached = this.idempotentStarts.get(options.idempotencyKey);
      if (cached !== undefined) {
        if (cached.fingerprint === fingerprint) {
          return cached.handle;
        }
        throw idempotencyConflict();
      }
    }
    const response = await this.transport.start({
      namespace,
      workflow_type: options.workflowType,
      input: options.input,
    });
    const workflowId = response.workflow_id?.uuid;
    const runId = response.run_id?.uuid;
    if (workflowId === undefined || runId === undefined) {
      throw mapWireError({
        code: "server",
        message: "Start response omitted workflow_id or run_id",
      });
    }
    const handle = new WorkflowHandle(this, workflowId, runId, namespace);
    if (options.idempotencyKey !== undefined && fingerprint !== undefined) {
      const recorded = this.idempotentStarts.get(options.idempotencyKey);
      if (recorded !== undefined && recorded.fingerprint !== fingerprint) {
        throw idempotencyConflict();
      }
      if (recorded === undefined) {
        this.idempotentStarts.set(options.idempotencyKey, {
          fingerprint,
          handle,
        });
      }
    }
    return handle;
  }

  async signal<I>(options: SignalOptions<I>): Promise<void> {
    await this.signalRaw({ ...options, payload: toPayload(options.payload) });
  }

  async signalRaw(options: SignalRawOptions): Promise<void> {
    await this.transport.signal({
      namespace: this.resolveNamespace(options.namespace),
      workflow_id: { uuid: options.workflowId },
      run_id: runId(options.runId),
      signal_name: options.signalName,
      payload: options.payload,
    });
  }

  async query<I, R>(options: QueryOptions<I>): Promise<R> {
    const raw = await this.queryRaw({
      ...options,
      input: options.input === undefined ? undefined : toPayload(options.input),
    });
    return fromPayload<R>(raw);
  }

  async queryRaw(options: QueryRawOptions): Promise<Payload> {
    const response = await this.queryWithDeadline(
      {
        namespace: this.resolveNamespace(options.namespace),
        workflow_id: { uuid: options.workflowId },
        run_id: runId(options.runId),
        query_name: options.queryName,
      },
      options.timeoutMs,
    );
    if (response.error !== undefined) {
      throw mapWireError(response.error);
    }
    if (response.result === undefined) {
      throw mapWireError({
        code: "server",
        message: "Query response omitted result and error",
      });
    }
    return response.result;
  }

  /**
   * Bounds one query round-trip by the caller's deadline. Query is a
   * synchronous deadline-bounded round-trip per the client contract:
   * deadline expiry surfaces {@link QueryTimeoutError} (the in-flight
   * transport call's eventual result is discarded).
   */
  private async queryWithDeadline(
    request: ProtoQueryRequest,
    timeoutMs?: number,
  ): Promise<ProtoQueryResponse> {
    if (timeoutMs === undefined) {
      return this.transport.query(request);
    }
    if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
      throw new InvalidArgumentError(
        `timeoutMs must be a positive number of milliseconds; got ${String(timeoutMs)}`,
      );
    }
    let timer: ReturnType<typeof setTimeout> | undefined;
    const deadline = new Promise<never>((_, reject) => {
      timer = setTimeout(() => {
        reject(
          new QueryTimeoutError(
            `Query deadline of ${String(timeoutMs)}ms elapsed before a result was available`,
          ),
        );
      }, timeoutMs);
    });
    const call = this.transport.query(request);
    // Once the deadline wins the race, the losing call's eventual rejection
    // must never surface as an unhandled rejection.
    void call.catch(() => undefined);
    try {
      return await Promise.race([call, deadline]);
    } finally {
      if (timer !== undefined) {
        clearTimeout(timer);
      }
    }
  }

  async cancel(options: CancelOptions): Promise<void> {
    await this.transport.cancel({
      namespace: this.resolveNamespace(options.namespace),
      workflow_id: { uuid: options.workflowId },
      run_id: runId(options.runId),
      reason: options.reason,
    });
  }

  async list(options: ListOptions = {}): Promise<ListWorkflowsResult> {
    const response = await this.transport.list({
      namespace: this.resolveNamespace(options.namespace),
      filter: options.filter,
    });
    return { summaries: response.summaries ?? [] };
  }

  async describe(options: DescribeOptions): Promise<WorkflowDescription> {
    const response = await this.transport.describe({
      namespace: this.resolveNamespace(options.namespace),
      workflow_id: { uuid: options.workflowId },
      run_id: runId(options.runId),
      include_history: options.includeHistory ?? false,
    });
    return {
      summary: response.summary,
      history: response.history ?? [],
    };
  }

  handle(
    workflowId: string,
    runId?: string,
    namespace?: string,
  ): WorkflowHandle {
    return new WorkflowHandle(
      this,
      workflowId,
      runId,
      this.resolveNamespace(namespace),
    );
  }

  resolveNamespace(namespace?: string): string {
    return namespace ?? this.namespace;
  }
}

class HttpWorkflowTransport implements WorkflowTransport {
  private readonly endpoint: string;
  private readonly auth?: AuthOptions;
  private readonly fetchImpl: typeof fetch;

  constructor(
    endpoint: string,
    auth: AuthOptions | undefined,
    fetchImpl: typeof fetch,
  ) {
    this.endpoint = endpoint;
    this.auth = auth;
    this.fetchImpl = fetchImpl;
  }

  async start(
    request: ProtoStartWorkflowRequest,
  ): Promise<ProtoStartWorkflowResponse> {
    return this.post<ProtoStartWorkflowResponse>("/workflows/start", request);
  }

  async signal(request: ProtoSignalRequest): Promise<void> {
    await this.post<unknown>("/workflows/signal", request);
  }

  async query(request: ProtoQueryRequest): Promise<ProtoQueryResponse> {
    // The HTTP route carries the QueryResponse outcome oneof as
    // `{"outcome":{"Result": <Payload>}}` or `{"outcome":{"Error":
    // <WireError>}}`; normalize it onto the transport-level result/error
    // shape the client decodes.
    const response = await this.post<{
      readonly outcome?: { readonly Result?: Payload; readonly Error?: unknown };
    }>("/workflows/query", request);
    if (response.outcome?.Error !== undefined) {
      return { error: response.outcome.Error };
    }
    return { result: response.outcome?.Result };
  }

  async cancel(request: ProtoCancelRequest): Promise<void> {
    await this.post<unknown>("/workflows/cancel", request);
  }

  async list(
    request: ProtoListWorkflowsRequest,
  ): Promise<ProtoListWorkflowsResponse> {
    return this.post<ProtoListWorkflowsResponse>("/workflows/list", request);
  }

  async describe(
    request: ProtoDescribeWorkflowRequest,
  ): Promise<ProtoDescribeWorkflowResponse> {
    return this.post<ProtoDescribeWorkflowResponse>(
      "/workflows/describe",
      request,
    );
  }

  private async post<T>(path: string, body: unknown): Promise<T> {
    let requestBody: string;
    try {
      requestBody = JSON.stringify(body);
    } catch (error) {
      throw new InvalidArgumentError("Failed to serialize request body", {
        cause: error,
      });
    }

    let response: Response;
    try {
      response = await this.fetchImpl(new URL(path, `${this.endpoint}/`), {
        method: "POST",
        headers: this.headers(),
        body: requestBody,
      });
    } catch (error) {
      throw mapTransportError(error);
    }

    if (!response.ok) {
      throw await mapHttpResponseError(response);
    }

    try {
      const text = await response.text();
      if (text.length === 0) {
        return undefined as T;
      }
      return JSON.parse(text) as T;
    } catch (error) {
      throw mapWireError({
        code: "server",
        message: "Failed to decode server response",
        detail: error,
      });
    }
  }

  private headers(): Record<string, string> {
    return {
      "content-type": "application/json",
      ...authHeaders(this.auth),
    };
  }
}

/**
 * The SDK-boundary idempotency conflict: the same key was reused with a
 * different start request.
 */
function idempotencyConflict(): AlreadyExistsError {
  return new AlreadyExistsError(
    "idempotency key was already used by a different start request " +
      "(namespace, workflow type, or input differ)",
  );
}

function normalizeEndpoint(endpoint: string, tls?: TlsOptions): string {
  let url: URL;
  try {
    url = new URL(endpoint);
  } catch (error) {
    throw new InvalidArgumentError("Client endpoint must be an absolute URL", {
      cause: error,
    });
  }
  if (tls?.enabled === true && url.protocol === "http:") {
    url.protocol = "https:";
  }
  return url.toString().replace(/\/$/, "");
}

function runId(value: string | undefined): ProtoRunId | undefined {
  return value === undefined ? undefined : { uuid: value };
}
