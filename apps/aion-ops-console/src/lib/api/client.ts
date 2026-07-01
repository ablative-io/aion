import type {
  ActivityId,
  ClusterCommand,
  ClusterSnapshot,
  Event,
  InterventionCapabilities,
  InterventionKind,
  InterventionOutcome,
  Namespace,
  NamespacePlacementWire,
  WorkflowFilter,
  WorkflowId,
  WorkflowSummary,
} from '@/types';

import { ApiError } from './api-error';
import {
  apiErrorFromResponse,
  type CreateNamespaceResult,
  type EventSearchResponse,
  type EventSearchResult,
  type HistoryResponse,
  type JsonRecord,
  type ListVersionsResponse,
  type LoadPackageResult,
  type NamespaceRecord,
  type NamespaceRecordsResponse,
  type NamespacesResponse,
  normalizeCreateNamespace,
  normalizeEventSearch,
  normalizeHistory,
  normalizeLoadPackage,
  normalizeNamespaceRecords,
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
  CreateNamespaceResult,
  EventSearchResult,
  JsonRecord,
  LoadPackageResult,
  NamespaceRecord,
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
  /**
   * Optional default task queue for this workflow's activities (the namespace ×
   * task_queue targeting story). Empty/absent = the namespace's default queue.
   */
  taskQueue?: string | undefined;
};

/**
 * One live, intervenable activity attempt of a workflow (NOI-7): the target
 * identity + the owning worker's advertised {@link InterventionCapabilities}. The
 * console gates controls on `capabilities.supported` — an empty set means the
 * attempt is observability-only and offers no controls.
 */
export type AttemptCapabilities = {
  activityId: ActivityId;
  attempt: number;
  capabilities: InterventionCapabilities;
};

/** Inputs to {@link ApiClient.intervene}: the target attempt + neutral primitive. */
export type InterveneParams = {
  workflowId: WorkflowId;
  activityId: ActivityId;
  attempt: number;
  /** The neutral control primitive (the ts-rs `InterventionKind`). */
  kind: InterventionKind;
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

/**
 * The caller's runtime capabilities, discovered from `GET /whoami`. The console
 * renders affordances (deploy panel, cross-namespace access) from THIS, never
 * from a build-time flag — authorization is a server decision made at request
 * time. In auth-off single-tenant operator mode the server reports
 * `authEnabled: false` with full access (`deployGranted` + `allNamespaces`).
 */
export type Capabilities = {
  /** Caller subject as resolved by the server (the audit label). */
  subject: string;
  /** Whether the server has auth configured. `false` ⇒ operator mode. */
  authEnabled: boolean;
  /** Whether the caller holds the deployment-wide deploy grant. */
  deployGranted: boolean;
  /** Whether the caller holds access to every namespace (operator mode). */
  allNamespaces: boolean;
  /** The caller's explicitly granted namespaces (empty for an operator). */
  namespaces: Namespace[];
};

/** Raw `/whoami` envelope (server snake_case), normalized into {@link Capabilities}. */
type WhoAmIResponse = {
  subject?: unknown;
  auth_enabled?: unknown;
  deploy_granted?: unknown;
  all_namespaces?: unknown;
  namespaces?: unknown;
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

  /**
   * Discover the caller's runtime capabilities (`GET /whoami`). This reflects
   * the server's request-time authorization decision for THIS caller, so the
   * console gates affordances on the result rather than on any build-time flag.
   * Runs through the same credential path as every request; in auth-off
   * operator mode no credentials are needed and the server returns full access.
   */
  async getCapabilities(options?: Pick<RequestOptions, 'credentials'>): Promise<Capabilities> {
    const response = await this.request<WhoAmIResponse>(
      AW_REST_CONTRACT.endpoints.whoami,
      AW_REST_CONTRACT.methods.whoami,
      { namespace: '' as Namespace, credentials: options?.credentials }
    );

    return normalizeCapabilities(response);
  }

  async listNamespaces(options?: Pick<RequestOptions, 'credentials'>): Promise<Namespace[]> {
    const response = await this.request<NamespacesResponse>(
      AW_REST_CONTRACT.endpoints.namespaces,
      AW_REST_CONTRACT.methods.namespaces,
      { namespace: '' as Namespace, credentials: options?.credentials }
    );

    return normalizeNamespaces(response);
  }

  /**
   * Explicitly create a namespace (`POST /namespaces`). Namespace-scoped: the
   * caller's `x-aion-namespaces` grant must include `name` (the SAME grant the
   * access path checks via `authorize_namespace`), so a caller can never create —
   * or learn the existence of — a namespace it cannot access. Idempotent server
   * side: `created === false` is the benign already-existed path, not an error.
   *
   * The credential scoping (target name in the namespace header) is the caller's
   * responsibility: pass a `credentials` whose namespaces include `name`, or use a
   * client built with `createConfiguredApiClient({ namespace: name })`. An empty
   * name (400), a denied grant (403), or any 4xx propagates as a typed
   * {@link ApiError} — never swallowed.
   */
  async createNamespace(
    name: string,
    options?: Pick<RequestOptions, 'credentials'>
  ): Promise<CreateNamespaceResult> {
    const response = await this.request<unknown>(
      AW_REST_CONTRACT.endpoints.namespaceCreate,
      AW_REST_CONTRACT.methods.namespaceCreate,
      { namespace: name as Namespace, credentials: options?.credentials },
      { [AW_REST_CONTRACT.requestKeys.namespaceName]: name }
    );

    return normalizeCreateNamespace(response);
  }

  /**
   * Fetch the durable namespace RECORDS (`GET /namespaces/records`) the caller
   * can see — the created/last_seen/origin columns the live namespace panel
   * renders. Grant-filtered + existence-leak-safe server-side, exactly like
   * {@link listNamespaces}; this returns the richer record shape rather than only
   * the names, so the panel can render columns without a second fetch.
   */
  async listNamespaceRecords(
    options?: Pick<RequestOptions, 'credentials'>
  ): Promise<NamespaceRecord[]> {
    const response = await this.request<NamespaceRecordsResponse>(
      AW_REST_CONTRACT.endpoints.namespaceRecords,
      AW_REST_CONTRACT.methods.namespaceRecords,
      { namespace: '' as Namespace, credentials: options?.credentials }
    );

    return normalizeNamespaceRecords(response);
  }

  /**
   * Set a namespace's durable placement directive (`PUT /namespaces/{name}/placement`).
   *
   * Namespace-scoped: the caller's `x-aion-namespaces` grant must include `name`
   * (the same grant the access path checks), so the console never sets — or learns
   * the existence of — a namespace it cannot access. The server runs a quorum
   * value-CAS on the record and, on success, emits a `NamespacePlacementChanged`
   * cluster delta; the panel folds that delta live, so this method does NOT return
   * the new placement — the socket is the source of truth (no refetch, no optimistic
   * client-side write that could diverge from the durable record). Rejection (an
   * unknown namespace, a denied grant, an invalid label set) propagates as a typed
   * {@link ApiError}.
   */
  async setNamespacePlacement(
    namespace: Namespace,
    placement: NamespacePlacementWire,
    options?: Pick<RequestOptions, 'credentials'>
  ): Promise<void> {
    const path = AW_REST_CONTRACT.endpoints.namespacePlacement.replace(
      '{name}',
      encodeURIComponent(namespace)
    );
    await this.request<unknown>(
      path,
      AW_REST_CONTRACT.methods.namespacePlacement,
      {
        // The grant is carried on the namespace header (buildHeaders reads
        // credentials.namespaces); the target namespace also travels in the path.
        namespace,
        credentials: options?.credentials,
      },
      {
        kind: placement.kind,
        nodes: placement.nodes,
      }
    );
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
    if (params.taskQueue !== undefined) {
      body.task_queue = params.taskQueue;
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
   * Enumerate a workflow's live intervenable activity attempts + their advertised
   * capabilities (`POST /workflows/attempts`). Namespace-scoped exactly like
   * {@link intervene}: the caller's namespace grant must cover the workflow.
   *
   * Only attempts with a LIVE owning worker are returned; a finished/superseded
   * attempt is absent (never offered a control). An empty array is the honest
   * answer for a workflow with no live agent attempt, NOT an error. The console
   * gates controls on each attempt's `capabilities.supported`, so it renders ONLY
   * the primitives the owning worker advertises. A denied grant or invalid request
   * propagates as a typed {@link ApiError} — never swallowed.
   */
  async listAttempts(
    workflowId: WorkflowId,
    options: RequestOptions
  ): Promise<AttemptCapabilities[]> {
    const response = await this.request<AttemptsResponseBody>(
      AW_REST_CONTRACT.endpoints.workflowAttempts,
      AW_REST_CONTRACT.methods.workflowAttempts,
      options,
      {
        [AW_REST_CONTRACT.requestKeys.namespace]: options.namespace,
        [AW_REST_CONTRACT.requestKeys.workflowId]: workflowId,
      }
    );

    return normalizeAttempts(response);
  }

  /**
   * Submit a mid-run intervention command (`POST /workflows/intervene`).
   * Namespace-scoped (ADR-022 per-namespace command authority, like signal/cancel).
   *
   * The server ALWAYS returns `200 OK` with a neutral {@link InterventionOutcome}
   * ack — `Applied`, `CapabilityNotSupported`, or `StaleTarget` — which this method
   * surfaces VERBATIM. A gated or stale ack is a first-class outcome the operator
   * inspects, NOT an error; only an authorization failure or a malformed request is
   * a typed {@link ApiError}. Errors are never swallowed and outcomes are never
   * reinterpreted as success. `issued_by`/`issued_at` are stamped server-side (the
   * console cannot forge attribution), so this body carries only the target +
   * primitive.
   */
  async intervene(params: InterveneParams, options: RequestOptions): Promise<InterventionOutcome> {
    const response = await this.request<InterveneResponseBody>(
      AW_REST_CONTRACT.endpoints.workflowIntervene,
      AW_REST_CONTRACT.methods.workflowIntervene,
      options,
      {
        [AW_REST_CONTRACT.requestKeys.namespace]: options.namespace,
        [AW_REST_CONTRACT.requestKeys.workflowId]: params.workflowId,
        activity_id: params.activityId,
        attempt: params.attempt,
        kind: params.kind,
      }
    );

    return readInterventionOutcome(response);
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
    // No build-time deploy header: deploy is authorized server-side in operator
    // mode, or by the bearer token's `deploy` claim under real auth. The console
    // never asserts the grant from a compiled flag.

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

/**
 * Normalize the server's `/whoami` envelope into {@link Capabilities}. Unknown
 * or missing fields collapse to the LEAST-privileged interpretation (no deploy,
 * no all-namespaces, auth treated as enabled) so a malformed response can never
 * spuriously unlock an affordance.
 */
function normalizeCapabilities(response: WhoAmIResponse): Capabilities {
  const namespaces = Array.isArray(response.namespaces)
    ? response.namespaces.filter((entry): entry is Namespace => typeof entry === 'string')
    : [];

  return {
    subject: typeof response.subject === 'string' ? response.subject : 'anonymous',
    // Default to auth-enabled (the safe assumption) when the flag is absent.
    authEnabled: response.auth_enabled !== false,
    deployGranted: response.deploy_granted === true,
    allNamespaces: response.all_namespaces === true,
    namespaces,
  };
}

/** Raw `/workflows/attempts` envelope (server snake_case). */
type AttemptsResponseBody = {
  attempts?: unknown;
};

/** Raw `/workflows/intervene` envelope (server snake_case). */
type InterveneResponseBody = {
  outcome?: unknown;
};

/**
 * Normalize the server's `/workflows/attempts` envelope into the console shape.
 * A row missing its load-bearing fields (activity/attempt/capabilities) is
 * DROPPED rather than surfaced as a phantom control target — the console never
 * offers a control for an attempt it cannot address. Malformed shapes throw a
 * typed {@link ApiError} rather than silently rendering nothing.
 */
function normalizeAttempts(response: AttemptsResponseBody): AttemptCapabilities[] {
  const rows = response.attempts;
  if (!Array.isArray(rows)) {
    throw new ApiError(200, 'workflows/attempts response missing an attempts array');
  }

  const attempts: AttemptCapabilities[] = [];
  for (const row of rows) {
    const attempt = readAttemptRow(row);
    if (attempt !== null) {
      attempts.push(attempt);
    }
  }
  return attempts;
}

/** Read one attempt row, or `null` when it lacks addressable target fields. */
function readAttemptRow(row: unknown): AttemptCapabilities | null {
  if (typeof row !== 'object' || row === null) {
    return null;
  }
  const record = row as Record<string, unknown>;
  const activityId = record.activity_id;
  const attempt = record.attempt;
  const capabilities = record.capabilities;
  if (
    typeof activityId !== 'number' ||
    typeof attempt !== 'number' ||
    !isCapabilities(capabilities)
  ) {
    return null;
  }
  return { activityId, attempt, capabilities };
}

/** Structural guard for the ts-rs `InterventionCapabilities` (a supported list). */
function isCapabilities(value: unknown): value is InterventionCapabilities {
  return (
    typeof value === 'object' &&
    value !== null &&
    Array.isArray((value as { supported?: unknown }).supported)
  );
}

/**
 * Read the neutral {@link InterventionOutcome} from the `/workflows/intervene`
 * envelope. An absent/malformed outcome is a real contract fault (a typed
 * {@link ApiError}), never quietly treated as a success.
 */
function readInterventionOutcome(response: InterveneResponseBody): InterventionOutcome {
  const outcome = response.outcome;
  if (
    typeof outcome === 'object' &&
    outcome !== null &&
    typeof (outcome as { outcome?: unknown }).outcome === 'string'
  ) {
    return outcome as InterventionOutcome;
  }
  throw new ApiError(200, 'workflows/intervene response missing a neutral outcome');
}

export function createApiClient(options?: ApiClientOptions): ApiClient {
  return new ApiClient(options);
}
