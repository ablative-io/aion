import type {
  ClusterCommand,
  ClusterSnapshot,
  Event,
  Namespace,
  WorkflowFilter,
  WorkflowId,
  WorkflowSummary,
} from '@/types';

import { ApiError } from './api-error';
import {
  apiErrorFromResponse,
  type EventSearchResponse,
  type EventSearchResult,
  type HistoryResponse,
  type JsonRecord,
  type ListVersionsResponse,
  type LoadPackageResult,
  type NamespacesResponse,
  normalizeEventSearch,
  normalizeHistory,
  normalizeLoadPackage,
  normalizeNamespaces,
  normalizeStartWorkflow,
  normalizeWorkflowPage,
  normalizeWorkflowVersions,
  readJson,
  type StartWorkflowResult,
  type WorkflowPage,
  type WorkflowQueryResponse,
  type WorkflowVersion,
} from './client-normalize';
import {
  type ApiCredentials,
  AW_REST_CONTRACT,
  appendHeaders,
  buildUrl,
  mergeCredentials,
  stripTrailingSlash,
  toBinaryBody,
} from './client-transport';

export type { ServerErrorBody } from './api-error';
export { ApiError } from './api-error';
export type {
  EventSearchResult,
  JsonRecord,
  LoadPackageResult,
  StartWorkflowResult,
  WorkflowPage,
  WorkflowVersion,
} from './client-normalize';
export type { ApiCredentials } from './client-transport';

const DEFAULT_LIMIT = 50;

/** Start-workflow inputs (camelCase); the body is built server-shaped below. */
export type StartWorkflowParams = {
  workflowType: string;
  /** Plain JSON input, auto-wrapped server-side as an `application/json` payload. */
  input?: JsonRecord | undefined;
  /** Optional R-4 steered-start routing key. */
  routingKey?: string | undefined;
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

  /**
   * Send an ADR-020 cluster command to `/cluster/command` (WS3 command seam).
   *
   * Cluster commands are deployment-scoped: the server deploy-gates them on the
   * caller's bearer/subject credentials (header-based — a browser CAN set headers
   * on a POST, so no query-param promotion is needed), never on a namespace
   * grant. Phase 1 ships exactly one real command — `RequestClusterSnapshot`, a
   * read returning the calm-state {@link ClusterSnapshot}. The mutating variants
   * compile so the contract exists, but the server runs the deploy gate first and
   * then returns a typed `Unimplemented` {@link ApiError} (zero blast radius); the
   * caller surfaces that to visible state rather than silently swallowing it.
   */
  async sendClusterCommand(command: ClusterCommand): Promise<ClusterSnapshot | null> {
    const response = await this.requestDeployScoped<ClusterSnapshot | null>(
      AW_REST_CONTRACT.endpoints.clusterCommand,
      'POST',
      command as unknown as JsonRecord
    );

    return response;
  }

  /** Convenience wrapper for the only Phase-1 mutating-free command. */
  async requestClusterSnapshot(): Promise<ClusterSnapshot> {
    const snapshot = await this.sendClusterCommand({ command: 'RequestClusterSnapshot' });

    if (snapshot === null) {
      throw new ApiError(200, 'cluster snapshot command returned an empty body');
    }

    return snapshot;
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

  /**
   * Start a workflow run (`POST /workflows/start`). Namespace-scoped: it carries
   * the per-namespace command authority (ADR-022), NOT the deploy grant. The
   * returned ids exist only because the server confirmed the run was created —
   * provenance is never fabricated; a 404 `WorkflowTypeNotFound` (type not
   * deployed) or 403 `namespace_denied` propagates as a typed {@link ApiError}.
   */
  async startWorkflow(
    params: StartWorkflowParams,
    options: RequestOptions
  ): Promise<StartWorkflowResult> {
    const body: JsonRecord = {
      [AW_REST_CONTRACT.requestKeys.namespace]: options.namespace,
      workflow_type: params.workflowType,
    };
    if (params.input !== undefined) {
      body.input = params.input;
    }
    if (params.routingKey !== undefined) {
      body.routing_key = params.routingKey;
    }

    const response = await this.request<unknown>(
      AW_REST_CONTRACT.endpoints.workflowStart,
      AW_REST_CONTRACT.methods.workflowStart,
      options,
      body
    );

    return normalizeStartWorkflow(response);
  }

  /**
   * Upload a `.aion` package archive (`POST /deploy/packages`). The whole request
   * body IS the archive bytes (raw `application/octet-stream`), not multipart or
   * JSON. Deployment-scoped: it carries the deploy grant (no namespace header).
   * When the cluster runs with `[deploy] enabled=false` this is a real 404; the
   * caller surfaces that honestly rather than pretending it succeeded.
   */
  async deployPackage(archive: ArrayBuffer | Uint8Array | Blob): Promise<LoadPackageResult> {
    const response = await this.requestDeployBinary<unknown>(
      AW_REST_CONTRACT.endpoints.deployPackages,
      toBinaryBody(archive)
    );

    return normalizeLoadPackage(response);
  }

  /** List loaded package versions (`GET /deploy/versions`). Deployment-scoped. */
  async listVersions(): Promise<WorkflowVersion[]> {
    const response = await this.requestDeployScoped<ListVersionsResponse>(
      AW_REST_CONTRACT.endpoints.deployVersions,
      AW_REST_CONTRACT.methods.deployVersions
    );

    return normalizeWorkflowVersions(response);
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

  /**
   * Issue a deployment-scoped request (no namespace). Used by the cluster
   * command seam: authorization is the deploy grant carried by the bearer/subject
   * credentials, so the namespace header is intentionally omitted.
   */
  private async requestDeployScoped<T>(
    path: string,
    method: string,
    body?: RequestBody
  ): Promise<T> {
    const headers = this.buildDeployHeaders('application/json');

    const init: RequestInit = { method, headers };
    if (body !== undefined) {
      init.body = JSON.stringify(body);
    }

    return this.sendDeploy<T>(path, init);
  }

  /**
   * Issue a deployment-scoped request whose body is a raw binary archive
   * (`application/octet-stream`) — used by the package upload. The archive is sent
   * verbatim (no `JSON.stringify`); the same deploy credentials as
   * {@link requestDeployScoped} apply (deploy grant, no namespace header).
   */
  private async requestDeployBinary<T>(path: string, archive: BodyInit): Promise<T> {
    const headers = this.buildDeployHeaders('application/octet-stream');

    return this.sendDeploy<T>(path, { method: 'POST', headers, body: archive });
  }

  private buildDeployHeaders(contentType: string): Headers {
    const headers = new Headers({ 'content-type': contentType });

    appendHeaders(headers, this.credentials?.headers);
    if (this.credentials?.bearerToken !== undefined) {
      headers.set('authorization', `Bearer ${this.credentials.bearerToken}`);
    }
    if (this.credentials?.subject !== undefined) {
      headers.set('x-aion-subject', this.credentials.subject);
    }
    // Dev/no-auth grant. Under real auth the server validates the bearer's
    // `deploy` claim and ignores this header, so emitting it is harmless there.
    if (this.credentials?.deployGranted === true) {
      headers.set('x-aion-deploy', 'true');
    }

    return headers;
  }

  private async sendDeploy<T>(path: string, init: RequestInit): Promise<T> {
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
