import {
  InvalidArgumentError,
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

export interface AuthOptions {
  readonly bearerToken?: string;
  readonly subject?: string;
  readonly namespaces?: readonly string[];
}

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
  readonly streamTransport?: SubscribeTransport;

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
    this.streamTransport = options.streamTransport;
  }

  async start<I>(options: StartOptions<I>): Promise<WorkflowHandle> {
    return this.startRaw({
      ...options,
      input: toPayload(options.input),
    });
  }

  async startRaw(options: StartRawOptions): Promise<WorkflowHandle> {
    const response = await this.transport.start({
      namespace: this.resolveNamespace(options.namespace),
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
    return new WorkflowHandle(
      this,
      workflowId,
      runId,
      this.resolveNamespace(options.namespace),
    );
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
    const response = await this.transport.query({
      namespace: this.resolveNamespace(options.namespace),
      workflow_id: { uuid: options.workflowId },
      run_id: runId(options.runId),
      query_name: options.queryName,
    });
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
    return this.post<ProtoQueryResponse>("/workflows/query", request);
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

  private headers(): HeadersInit {
    const headers: Record<string, string> = {
      "content-type": "application/json",
    };
    if (this.auth?.bearerToken !== undefined) {
      headers.authorization = `Bearer ${this.auth.bearerToken}`;
    }
    if (this.auth?.subject !== undefined) {
      headers["x-aion-subject"] = this.auth.subject;
    }
    if (this.auth?.namespaces !== undefined) {
      headers["x-aion-namespaces"] = this.auth.namespaces.join(",");
    }
    return headers;
  }
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
