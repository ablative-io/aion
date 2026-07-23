import type {
  ClusterCommand,
  ClusterSnapshot,
  Event,
  InterventionOutcome,
  Namespace,
  NamespacePlacementWire,
  RunId,
  WorkflowFilter,
  WorkflowId,
  WorkflowSummary,
} from '@/types';

import { ApiError } from './api-error';
import {
  type AttemptsResponseBody,
  type InterveneResponseBody,
  normalizeAttempts,
  normalizeCapabilities,
  readInterventionOutcome,
} from './client-action-normalize';
import {
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
  normalizeReopenWorkflow,
  normalizeStartWorkflow,
  normalizeWorkflowPage,
  normalizeWorkflowVersions,
  type ReopenWorkflowResult,
  type StartWorkflowResult,
  type WorkflowPage,
  type WorkflowQueryResponse,
  type WorkflowVersion,
} from './client-normalize';
import { ApiRequestTransport } from './client-request';
import { AW_REST_CONTRACT, toBinaryBody } from './client-transport';
import type {
  ApiClientOptions,
  AttemptCapabilities,
  Capabilities,
  EventSearchQuery,
  InterveneParams,
  RequestOptions,
  StartWorkflowParams,
  WhoAmIResponse,
  WorkflowPageRequest,
} from './client-types';
import { requestHistoryWindow, requestWorkflowEvent } from './workflow-history-client';
import type { HistoryWindow, HistoryWindowRequest } from './workflow-history-contract';

export type { ServerErrorBody } from './api-error';
export { ApiError } from './api-error';
export type {
  CreateNamespaceResult,
  EventSearchResult,
  JsonRecord,
  LoadPackageResult,
  NamespaceRecord,
  ReopenWorkflowResult,
  StartWorkflowResult,
  WorkflowPage,
  WorkflowVersion,
} from './client-normalize';
export type { ApiCredentials } from './client-transport';
export type {
  ApiClientOptions,
  AttemptCapabilities,
  Capabilities,
  EventSearchQuery,
  InterveneParams,
  RequestOptions,
  StartWorkflowParams,
  WorkflowPageRequest,
} from './client-types';
export type { HistoryWindow, HistoryWindowRequest } from './workflow-history-contract';

const DEFAULT_LIMIT = 50;

export class ApiClient {
  private readonly transport: ApiRequestTransport;

  constructor(options: ApiClientOptions = {}) {
    this.transport = new ApiRequestTransport(options);
  }

  async queryWorkflows(
    filter: WorkflowFilter,
    page: WorkflowPageRequest,
    options: RequestOptions
  ): Promise<WorkflowPage<WorkflowSummary>> {
    const body = this.buildWorkflowQueryBody(filter, page, options.namespace);
    const response = await this.transport.request<WorkflowQueryResponse>(
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
    const response = await this.transport.request<HistoryResponse>(
      AW_REST_CONTRACT.endpoints.history,
      AW_REST_CONTRACT.methods.history,
      options,
      this.buildHistoryBody(workflowId, options.namespace)
    );

    return normalizeHistory(response);
  }

  /** Fetch an elision-aware, ascending window of durable workflow history. */
  async getHistoryWindow(
    workflowId: WorkflowId,
    window: HistoryWindowRequest,
    options: RequestOptions
  ): Promise<HistoryWindow> {
    return requestHistoryWindow(this.transport, workflowId, window, options);
  }

  /** Fetch one full durable event. The server never elides this response. */
  getEvent(workflowId: WorkflowId, seq: number, options: RequestOptions): Promise<Event> {
    return requestWorkflowEvent(this.transport, workflowId, seq, options);
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
    const response = await this.transport.request<EventSearchResponse>(
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
    const response = await this.transport.requestDeployScoped<ClusterSnapshot | null>(
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
    const response = await this.transport.request<WhoAmIResponse>(
      AW_REST_CONTRACT.endpoints.whoami,
      AW_REST_CONTRACT.methods.whoami,
      { namespace: '' as Namespace, credentials: options?.credentials }
    );

    return normalizeCapabilities(response);
  }

  async listNamespaces(options?: Pick<RequestOptions, 'credentials'>): Promise<Namespace[]> {
    const response = await this.transport.request<NamespacesResponse>(
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
    const response = await this.transport.request<unknown>(
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
    const response = await this.transport.request<NamespaceRecordsResponse>(
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
    await this.transport.request<unknown>(
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
    const response = await this.transport.request<
      WorkflowSummary[] | { items?: WorkflowSummary[] }
    >(
      `${AW_REST_CONTRACT.endpoints.workflowsPlain}?${AW_REST_CONTRACT.requestKeys.namespace}=${encodeURIComponent(options.namespace)}`,
      AW_REST_CONTRACT.methods.workflowsPlain,
      options
    );

    return Array.isArray(response) ? response : (response.items ?? []);
  }

  async countWorkflows(options: RequestOptions): Promise<number> {
    const response = await this.transport.request<{ count?: number } | number>(
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

    const response = await this.transport.request<unknown>(
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
    const response = await this.transport.request<AttemptsResponseBody>(
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
    const response = await this.transport.request<InterveneResponseBody>(
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
   * Reopen a terminal (Failed/Cancelled) workflow (`POST /workflows/reopen`).
   * Namespace-scoped exactly like cancel/signal (ADR-022 per-namespace command
   * authority). Re-dispatches the failed tail and returns the reopened run id +
   * its projected status (Running immediately after reopen).
   *
   * A non-reopenable-terminal run (`invalid_state`) or an absent workflow
   * (`not_found`) surfaces as a typed {@link ApiError} — never swallowed and
   * never reinterpreted as success. `run_id` is omitted to target the latest run
   * (the server resolves it), matching the CLI's `aion reopen <id>`.
   */
  async reopen(
    workflowId: WorkflowId,
    options: RequestOptions,
    runId?: RunId
  ): Promise<ReopenWorkflowResult> {
    const response = await this.transport.request<unknown>(
      AW_REST_CONTRACT.endpoints.workflowReopen,
      AW_REST_CONTRACT.methods.workflowReopen,
      options,
      {
        [AW_REST_CONTRACT.requestKeys.namespace]: options.namespace,
        [AW_REST_CONTRACT.requestKeys.workflowId]: workflowId,
        ...(runId === undefined ? {} : { [AW_REST_CONTRACT.requestKeys.runId]: runId }),
      }
    );

    return normalizeReopenWorkflow(response);
  }

  /**
   * Deliver a named signal to a running workflow (`POST /workflows/signal`).
   * Namespace-scoped exactly like cancel/reopen (ADR-022 per-namespace command
   * authority). The plain-JSON `payload` is auto-wrapped server-side as an
   * `application/json` payload (the same start-input path); omit it for a
   * payload-less signal. `run_id` is omitted to target the latest run (the
   * server resolves it), matching the CLI's `aion signal <id> <name>`.
   *
   * The server ack body is empty (`SignalResponse {}`), so success is the 2xx
   * itself; a missing workflow (`not_found`), a denied grant, or an invalid id
   * propagates as a typed {@link ApiError} — never swallowed.
   */
  async sendSignal(
    workflowId: WorkflowId,
    signalName: string,
    options: RequestOptions,
    payload?: JsonRecord,
    runId?: RunId
  ): Promise<void> {
    await this.transport.request<unknown>(
      AW_REST_CONTRACT.endpoints.workflowSignal,
      AW_REST_CONTRACT.methods.workflowSignal,
      options,
      {
        [AW_REST_CONTRACT.requestKeys.namespace]: options.namespace,
        [AW_REST_CONTRACT.requestKeys.workflowId]: workflowId,
        [AW_REST_CONTRACT.requestKeys.signalName]: signalName,
        ...(payload === undefined ? {} : { [AW_REST_CONTRACT.requestKeys.payload]: payload }),
        ...(runId === undefined ? {} : { [AW_REST_CONTRACT.requestKeys.runId]: runId }),
      }
    );
  }

  /**
   * Upload a `.aion` package archive (`POST /deploy/packages`). The whole request
   * body IS the archive bytes (raw `application/octet-stream`), not multipart or
   * JSON. Deployment-scoped: it carries the deploy grant (no namespace header).
   * When the cluster runs with `[deploy] enabled=false` this is a real 404; the
   * caller surfaces that honestly rather than pretending it succeeded.
   */
  async deployPackage(archive: ArrayBuffer | Uint8Array | Blob): Promise<LoadPackageResult> {
    const response = await this.transport.requestDeployBinary<unknown>(
      AW_REST_CONTRACT.endpoints.deployPackages,
      toBinaryBody(archive)
    );

    return normalizeLoadPackage(response);
  }

  /** List loaded package versions (`GET /deploy/versions`). Deployment-scoped. */
  async listVersions(): Promise<WorkflowVersion[]> {
    const response = await this.transport.requestDeployScoped<ListVersionsResponse>(
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
}

export function createApiClient(options?: ApiClientOptions): ApiClient {
  return new ApiClient(options);
}
