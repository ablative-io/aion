import type { Event, Namespace, WorkflowFilter, WorkflowId, WorkflowSummary } from '@/types';

import { ApiError } from './api-error';
import {
  apiErrorFromResponse,
  type EventSearchResponse,
  type EventSearchResult,
  type HistoryResponse,
  type JsonRecord,
  type NamespacesResponse,
  normalizeEventSearch,
  normalizeHistory,
  normalizeNamespaces,
  normalizeWorkflowPage,
  readJson,
  type WorkflowPage,
  type WorkflowQueryResponse,
} from './client-normalize';

export type { ServerErrorBody } from './api-error';
export { ApiError } from './api-error';
export type { EventSearchResult, WorkflowPage } from './client-normalize';

const DEFAULT_LIMIT = 50;

// AW REST contract surface: update this one object when cluster AW pins endpoint paths,
// methods, request body keys, pagination names, or envelope response shapes.
const AW_REST_CONTRACT = {
  endpoints: {
    workflows: '/workflows/list',
    workflowsPlain: '/workflows',
    workflowsCount: '/workflows/count',
    history: '/workflows/describe',
    namespaces: '/namespaces',
    eventSearch: '/events/search',
  },
  methods: {
    workflows: 'POST',
    history: 'POST',
    namespaces: 'GET',
    workflowsPlain: 'GET',
    workflowsCount: 'GET',
    eventSearch: 'POST',
  },
  requestKeys: {
    namespace: 'namespace',
    filter: 'filter',
    workflowId: 'workflow_id',
    runId: 'run_id',
    includeHistory: 'include_history',
    query: 'query',
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
    results: 'results',
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

type FetchFn = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

export type ApiClientOptions = {
  baseUrl?: string;
  fetchImpl?: FetchFn;
  credentials?: ApiCredentials;
};

export type RequestOptions = {
  namespace: Namespace;
  credentials?: ApiCredentials | undefined;
};

export type WorkflowPageRequest = {
  cursor?: string | undefined;
  limit?: number | undefined;
};

/**
 * Field-aware event-search query (plan §4.5 / slice S8). All fields are
 * optional and AND-combined server-side; an empty query is rejected by the
 * caller, not silently treated as "match all".
 */
export type EventSearchQuery = {
  /** Match a specific event variant (`Event['type']`), e.g. "ActivityFailed". */
  eventType?: string;
  /** Match the workflow type the event belongs to. */
  workflowType?: string;
  /** Match an activity type (for activity events). */
  activityType?: string;
  /** Substring match against an error message / kind. */
  errorText?: string;
  /** Lower bound (inclusive) on the event's recorded_at, ISO-8601. */
  recordedAfter?: string;
  /** Upper bound (inclusive) on the event's recorded_at, ISO-8601. */
  recordedBefore?: string;
};

type RequestBody = JsonRecord | undefined;

export class ApiClient {
  private readonly baseUrl: string;
  private readonly fetchImpl: FetchFn;
  private readonly credentials?: ApiCredentials;

  constructor(options: ApiClientOptions = {}) {
    this.baseUrl = stripTrailingSlash(options.baseUrl ?? '');
    // Default to a fetch whose `this` is bound to the realm's global. Storing the
    // bare global `fetch` and calling it as `this.fetchImpl(...)` would invoke it
    // with the wrong receiver and throw `TypeError: Illegal invocation` at runtime;
    // an explicitly-bound wrapper keeps the default correct while still allowing an
    // injected fetchImpl (e.g. in tests).
    this.fetchImpl = options.fetchImpl ?? ((input, init) => globalThis.fetch(input, init));
    if (options.credentials !== undefined) {
      this.credentials = options.credentials;
    }
  }

  async queryWorkflows(
    filter: WorkflowFilter,
    page: WorkflowPageRequest,
    options: RequestOptions
  ): Promise<WorkflowPage<WorkflowSummary>> {
    const body = this.buildWorkflowQueryBody(filter, page, options.namespace);
    const response = await this.request<WorkflowQueryResponse>(
      AW_REST_CONTRACT.endpoints.workflows,
      AW_REST_CONTRACT.methods.workflows,
      options,
      body
    );

    return normalizeWorkflowPage(response, page.limit ?? DEFAULT_LIMIT);
  }

  async listWorkflows(
    filter: WorkflowFilter,
    page: WorkflowPageRequest,
    options: RequestOptions
  ): Promise<WorkflowPage<WorkflowSummary>> {
    return this.queryWorkflows(filter, page, options);
  }

  async getHistory(workflowId: WorkflowId, options: RequestOptions): Promise<Event[]> {
    const response = await this.request<HistoryResponse>(
      AW_REST_CONTRACT.endpoints.history,
      AW_REST_CONTRACT.methods.history,
      options,
      this.buildHistoryBody(workflowId, options.namespace)
    );

    return normalizeHistory(response);
  }

  /**
   * Field-aware event search (plan §4.5 / slice S8). Posts the query to the AW
   * event-search endpoint and normalizes the result envelope. The server search
   * surface is not pinned yet: when the endpoint is absent the request throws a
   * real {@link ApiError} (e.g. 404) which the caller surfaces to visible state —
   * it never returns fabricated or empty-but-silent results.
   */
  async searchEvents(
    query: EventSearchQuery,
    page: WorkflowPageRequest,
    options: RequestOptions
  ): Promise<WorkflowPage<EventSearchResult>> {
    const response = await this.request<EventSearchResponse>(
      AW_REST_CONTRACT.endpoints.eventSearch,
      AW_REST_CONTRACT.methods.eventSearch,
      options,
      this.buildEventSearchBody(query, page, options.namespace)
    );

    return normalizeEventSearch(response, page.limit ?? DEFAULT_LIMIT);
  }

  async listNamespaces(options?: Pick<RequestOptions, 'credentials'>): Promise<Namespace[]> {
    const response = await this.request<NamespacesResponse>(
      AW_REST_CONTRACT.endpoints.namespaces,
      AW_REST_CONTRACT.methods.namespaces,
      { namespace: '' as Namespace, credentials: options?.credentials }
    );

    return normalizeNamespaces(response);
  }

  async getWorkflowsPlain(options: RequestOptions): Promise<WorkflowSummary[]> {
    const response = await this.request<WorkflowSummary[] | { items?: WorkflowSummary[] }>(
      `${AW_REST_CONTRACT.endpoints.workflowsPlain}?${AW_REST_CONTRACT.requestKeys.namespace}=${encodeURIComponent(options.namespace)}`,
      AW_REST_CONTRACT.methods.workflowsPlain,
      options
    );

    return Array.isArray(response) ? response : (response.items ?? []);
  }

  async countWorkflows(options: RequestOptions): Promise<number> {
    const response = await this.request<{ count?: number } | number>(
      `${AW_REST_CONTRACT.endpoints.workflowsCount}?${AW_REST_CONTRACT.requestKeys.namespace}=${encodeURIComponent(options.namespace)}`,
      AW_REST_CONTRACT.methods.workflowsCount,
      options
    );

    const count = typeof response === 'number' ? response : response.count;

    if (typeof count !== 'number') {
      throw new ApiError(200, 'workflows/count response missing numeric count');
    }

    return count;
  }

  private buildWorkflowQueryBody(
    filter: WorkflowFilter,
    page: WorkflowPageRequest,
    namespace: Namespace
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

  private buildEventSearchBody(
    query: EventSearchQuery,
    page: WorkflowPageRequest,
    namespace: Namespace
  ): JsonRecord {
    return {
      [AW_REST_CONTRACT.requestKeys.namespace]: namespace,
      [AW_REST_CONTRACT.requestKeys.query]: query,
      [AW_REST_CONTRACT.requestKeys.pagination.cursor]: page.cursor ?? null,
      [AW_REST_CONTRACT.requestKeys.pagination.limit]: page.limit ?? DEFAULT_LIMIT,
    };
  }

  private async request<T>(
    path: string,
    method: string,
    options: RequestOptions,
    body?: RequestBody
  ): Promise<T> {
    const init: RequestInit = {
      method,
      headers: this.buildHeaders(options),
    };

    if (body !== undefined) {
      init.body = JSON.stringify(body);
    }

    const response = await this.fetchImpl(buildUrl(this.baseUrl, path), init);

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

function buildUrl(baseUrl: string, path: string): string {
  return `${baseUrl}${path}`;
}

function stripTrailingSlash(value: string): string {
  return value.endsWith('/') ? value.slice(0, -1) : value;
}

function mergeCredentials(
  base: ApiCredentials | undefined,
  override: ApiCredentials | undefined
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
  override: HeadersInit | undefined
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
