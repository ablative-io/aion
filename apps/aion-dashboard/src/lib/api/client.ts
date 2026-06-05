import type {
  Event,
  Namespace,
  WorkflowFilter,
  WorkflowId,
  WorkflowSummary,
} from '@/types';

const DEFAULT_LIMIT = 50;

// AW REST contract surface: update this one object when cluster AW pins endpoint paths,
// methods, request body keys, pagination names, or envelope response shapes.
const AW_REST_CONTRACT = {
  endpoints: {
    workflows: '/workflows/list',
    history: '/workflows/describe',
    namespaces: '/namespaces',
  },
  methods: {
    workflows: 'POST',
    history: 'POST',
    namespaces: 'GET',
  },
  requestKeys: {
    namespace: 'namespace',
    filter: 'filter',
    workflowId: 'workflow_id',
    runId: 'run_id',
    includeHistory: 'include_history',
    pagination: {
      cursor: 'cursor',
      limit: 'limit',
    },
  },
  responseKeys: {
    items: 'items',
    summaries: 'summaries',
    events: 'events',
    history: 'history',
    namespaces: 'namespaces',
    nextCursor: 'next_cursor',
    hasMore: 'has_more',
    payload: 'payload',
    payloadBytes: 'bytes',
  },
} as const;

export type ApiCredentials = {
  bearerToken?: string;
  subject?: string;
  namespaces?: readonly Namespace[];
  headers?: HeadersInit;
};

export type ApiClientOptions = {
  baseUrl?: string;
  fetchImpl?: typeof fetch;
  credentials?: ApiCredentials;
};

export type RequestOptions = {
  namespace: Namespace;
  credentials?: ApiCredentials;
};

export type WorkflowPageRequest = {
  cursor?: string;
  limit?: number;
};

export type WorkflowPage<T> = {
  items: T[];
  nextCursor: string | null;
  hasMore: boolean;
};

export type ServerErrorBody = {
  code?: string;
  message?: string;
};

export class ApiError extends Error {
  readonly status: number;
  readonly code: string | null;

  constructor(status: number, message: string, code: string | null = null) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
  }
}

type JsonRecord = Record<string, unknown>;

type RequestBody = JsonRecord | undefined;

type WorkflowQueryResponse =
  | WorkflowSummary[]
  | {
      items?: WorkflowSummary[];
      summaries?: unknown[];
      next_cursor?: string | null;
      has_more?: boolean;
    };

type HistoryResponse =
  | Event[]
  | {
      events?: unknown[];
      history?: unknown[];
    };

type NamespacesResponse = Namespace[] | { namespaces?: Namespace[] };

export class ApiClient {
  private readonly baseUrl: string;
  private readonly fetchImpl: typeof fetch;
  private readonly credentials?: ApiCredentials;

  constructor(options: ApiClientOptions = {}) {
    this.baseUrl = stripTrailingSlash(options.baseUrl ?? '');
    this.fetchImpl = options.fetchImpl ?? fetch;
    this.credentials = options.credentials;
  }

  async queryWorkflows(
    filter: WorkflowFilter,
    page: WorkflowPageRequest,
    options: RequestOptions,
  ): Promise<WorkflowPage<WorkflowSummary>> {
    const body = this.buildWorkflowQueryBody(filter, page, options.namespace);
    const response = await this.request<WorkflowQueryResponse>(
      AW_REST_CONTRACT.endpoints.workflows,
      AW_REST_CONTRACT.methods.workflows,
      options,
      body,
    );

    return normalizeWorkflowPage(response, page.limit ?? DEFAULT_LIMIT);
  }

  async listWorkflows(
    filter: WorkflowFilter,
    page: WorkflowPageRequest,
    options: RequestOptions,
  ): Promise<WorkflowPage<WorkflowSummary>> {
    return this.queryWorkflows(filter, page, options);
  }

  async getHistory(workflowId: WorkflowId, options: RequestOptions): Promise<Event[]> {
    const response = await this.request<HistoryResponse>(
      AW_REST_CONTRACT.endpoints.history,
      AW_REST_CONTRACT.methods.history,
      options,
      this.buildHistoryBody(workflowId, options.namespace),
    );

    return normalizeHistory(response);
  }

  async listNamespaces(options?: Pick<RequestOptions, 'credentials'>): Promise<Namespace[]> {
    const response = await this.request<NamespacesResponse>(
      AW_REST_CONTRACT.endpoints.namespaces,
      AW_REST_CONTRACT.methods.namespaces,
      { namespace: '' as Namespace, credentials: options?.credentials },
    );

    return Array.isArray(response)
      ? response
      : readArray<Namespace>(response, AW_REST_CONTRACT.responseKeys.namespaces);
  }

  private buildWorkflowQueryBody(
    filter: WorkflowFilter,
    page: WorkflowPageRequest,
    namespace: Namespace,
  ): JsonRecord {
    return {
      [AW_REST_CONTRACT.requestKeys.namespace]: namespace,
      [AW_REST_CONTRACT.requestKeys.filter]: filter,
      [AW_REST_CONTRACT.requestKeys.pagination.cursor]: page.cursor ?? null,
      [AW_REST_CONTRACT.requestKeys.pagination.limit]: page.limit ?? DEFAULT_LIMIT,
    };
  }

  private buildHistoryBody(workflowId: WorkflowId, namespace: Namespace): JsonRecord {
    return {
      [AW_REST_CONTRACT.requestKeys.namespace]: namespace,
      [AW_REST_CONTRACT.requestKeys.workflowId]: workflowId,
      [AW_REST_CONTRACT.requestKeys.runId]: null,
      [AW_REST_CONTRACT.requestKeys.includeHistory]: true,
    };
  }

  private async request<T>(
    path: string,
    method: string,
    options: RequestOptions,
    body?: RequestBody,
  ): Promise<T> {
    const response = await this.fetchImpl(buildUrl(this.baseUrl, path), {
      method,
      headers: this.buildHeaders(options),
      body: body === undefined ? undefined : JSON.stringify(body),
    });

    if (!response.ok) {
      const errorBody = await readJson(response).catch(() => null);
      throw apiErrorFromResponse(response.status, errorBody);
    }

    const json = await readJson(response);
    return json as T;
  }

  private buildHeaders(options: RequestOptions): Headers {
    const headers = new Headers({ 'content-type': 'application/json' });
    const credentials = mergeCredentials(this.credentials, options.credentials);

    appendHeaders(headers, credentials?.headers);

    if (credentials?.bearerToken !== undefined) {
      headers.set('authorization', `Bearer ${credentials.bearerToken}`);
    }

    if (credentials?.subject !== undefined) {
      headers.set('x-aion-subject', credentials.subject);
    }

    if (credentials?.namespaces !== undefined) {
      headers.set('x-aion-namespaces', credentials.namespaces.join(','));
    }

    return headers;
  }
}

export function createApiClient(options?: ApiClientOptions): ApiClient {
  return new ApiClient(options);
}

function normalizeWorkflowPage(
  response: WorkflowQueryResponse,
  requestedLimit: number,
): WorkflowPage<WorkflowSummary> {
  if (Array.isArray(response)) {
    return {
      items: response,
      nextCursor: null,
      hasMore: response.length >= requestedLimit,
    };
  }

  const items = response.items ?? readEnvelopeArray<WorkflowSummary>(response.summaries ?? []);

  return {
    items,
    nextCursor: response.next_cursor ?? null,
    hasMore: response.has_more ?? response.next_cursor !== undefined,
  };
}

function normalizeHistory(response: HistoryResponse): Event[] {
  const events = Array.isArray(response)
    ? response
    : response.events ?? readEnvelopeArray<Event>(response.history ?? []);

  return [...events].sort((left, right) => left.data.envelope.seq - right.data.envelope.seq);
}

function readEnvelopeArray<T>(values: unknown[]): T[] {
  return values.map((value) => readEnvelopePayload<T>(value));
}

function readEnvelopePayload<T>(value: unknown): T {
  if (!isRecord(value)) {
    return value as T;
  }

  const payload = value[AW_REST_CONTRACT.responseKeys.payload];

  if (!isRecord(payload)) {
    return value as T;
  }

  const bytes = payload[AW_REST_CONTRACT.responseKeys.payloadBytes];

  if (Array.isArray(bytes)) {
    return JSON.parse(String.fromCharCode(...bytes)) as T;
  }

  if (typeof bytes === 'string') {
    return JSON.parse(decodeBase64(bytes)) as T;
  }

  return value as T;
}

function readArray<T>(record: JsonRecord, key: string): T[] {
  const value = record[key];
  return Array.isArray(value) ? (value as T[]) : [];
}

async function readJson(response: Response): Promise<unknown> {
  const text = await response.text();

  if (text.length === 0) {
    return null;
  }

  return JSON.parse(text) as unknown;
}

function apiErrorFromResponse(status: number, body: unknown): ApiError {
  if (isRecord(body)) {
    const maybeCode = body.code;
    const maybeMessage = body.message;
    const message =
      typeof maybeMessage === 'string' ? maybeMessage : `Request failed with ${status}`;
    const code = typeof maybeCode === 'string' ? maybeCode : null;

    return new ApiError(status, message, code);
  }

  return new ApiError(status, `Request failed with ${status}`);
}

function buildUrl(baseUrl: string, path: string): string {
  return `${baseUrl}${path}`;
}

function stripTrailingSlash(value: string): string {
  return value.endsWith('/') ? value.slice(0, -1) : value;
}

function mergeCredentials(
  base: ApiCredentials | undefined,
  override: ApiCredentials | undefined,
): ApiCredentials | undefined {
  if (base === undefined) {
    return override;
  }

  if (override === undefined) {
    return base;
  }

  return {
    ...base,
    ...override,
    headers: mergeHeaderInputs(base.headers, override.headers),
  };
}

function mergeHeaderInputs(
  base: HeadersInit | undefined,
  override: HeadersInit | undefined,
): Headers {
  const headers = new Headers(base);
  appendHeaders(headers, override);
  return headers;
}

function appendHeaders(headers: Headers, input: HeadersInit | undefined): void {
  if (input === undefined) {
    return;
  }

  new Headers(input).forEach((value, key) => {
    headers.set(key, value);
  });
}

function isRecord(value: unknown): value is JsonRecord {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function decodeBase64(value: string): string {
  if (typeof atob === 'function') {
    return atob(value);
  }

  return Buffer.from(value, 'base64').toString('utf8');
}
