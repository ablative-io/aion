import type {
  Event,
  Namespace,
  NamespacePlacementWire,
  WorkflowId,
  WorkflowSummary,
} from '@/types';

import { ApiError } from './api-error';

// Response envelope keys (mirror of AW_REST_CONTRACT.responseKeys). Kept here so
// the normalization helpers are a self-contained module and client.ts stays
// under the 500-line house limit. Update alongside the contract in client.ts.
const PAYLOAD_KEY = 'payload';
const PAYLOAD_BYTES_KEY = 'bytes';
const NAMESPACES_KEY = 'namespaces';

export type JsonRecord = Record<string, unknown>;

export type WorkflowPage<T> = {
  items: T[];
  nextCursor: string | null;
  hasMore: boolean;
};

/** One event-search hit: the matched event plus its locating coordinates. */
export type EventSearchResult = {
  event: Event;
  workflowId: WorkflowId;
  seq: number;
};

export type WorkflowQueryResponse =
  | WorkflowSummary[]
  | {
      items?: WorkflowSummary[];
      summaries?: unknown[];
      next_cursor?: string | null;
      has_more?: boolean;
    };

export type HistoryResponse =
  | Event[]
  | {
      events?: unknown[];
      history?: unknown[];
    };

export type NamespacesResponse = Namespace[] | { namespaces?: Namespace[] };

/**
 * One durable namespace registry row as the console renders it (the columns of
 * the live namespace panel). The wire shape is `NamespaceRecordSummary` from
 * `GET /namespaces/records`: name + RFC 3339 created_at/last_seen + the stable
 * snake_case origin label. `origin` is kept as the raw server label (not a
 * narrowed union) so a future server-side origin variant never silently fails to
 * parse — the UI maps known labels to friendly text and falls back to the raw
 * label for anything unrecognized.
 */
export type NamespaceRecord = {
  name: Namespace;
  createdAt: string;
  lastSeen: string;
  origin: string;
  /**
   * The namespace's durable placement directive (Control-Plane Phase 2, P2-I2),
   * as the stable `{kind, nodes}` wire projection: `unplaced` (default), `prefer`
   * (soft/spill), or `pinned` (hard/isolated). Sourced from the durable record so
   * the panel renders current placement on mount, then reconciled by value against
   * a live `NamespacePlacementChanged` delta.
   */
  placement: NamespacePlacementWire;
};

/** Raw `GET /namespaces/records` row, before camelCase normalization. */
type RawNamespaceRecord = {
  name?: unknown;
  created_at?: unknown;
  last_seen?: unknown;
  origin?: unknown;
  placement?: unknown;
};

export type NamespaceRecordsResponse = RawNamespaceRecord[] | { namespaces?: RawNamespaceRecord[] };

/**
 * Normalize the `GET /namespaces/records` response into the camelCase
 * {@link NamespaceRecord} shape. Accepts either a bare array (the server's actual
 * shape) or a `{ namespaces }` envelope, mirroring {@link normalizeNamespaces}'s
 * tolerance. A row missing its required `name` is a contract violation surfaced
 * as an {@link ApiError}, never silently dropped.
 */
export function normalizeNamespaceRecords(response: NamespaceRecordsResponse): NamespaceRecord[] {
  const rows: RawNamespaceRecord[] = Array.isArray(response)
    ? response
    : readArray<RawNamespaceRecord>(response, NAMESPACES_KEY);

  return rows.map((row) => ({
    name: requireString(row.name, 'name') as Namespace,
    createdAt: requireString(row.created_at, 'created_at'),
    lastSeen: requireString(row.last_seen, 'last_seen'),
    origin: asString(row.origin),
    placement: normalizePlacement(row.placement),
  }));
}

/**
 * Normalize a raw `placement` field into the {@link NamespacePlacementWire} shape.
 * A missing/malformed placement resolves to `unplaced` (the record default) rather
 * than throwing — placement is a policy overlay, never a required identity field,
 * so a pre-Phase-2 record with no placement key renders as the default, never an
 * error. An unrecognized `kind` string is preserved verbatim so a future server
 * variant renders raw instead of silently collapsing to the default.
 */
export function normalizePlacement(value: unknown): NamespacePlacementWire {
  if (!isRecord(value)) {
    return { kind: 'unplaced', nodes: [] };
  }
  const kind = typeof value.kind === 'string' && value.kind.length > 0 ? value.kind : 'unplaced';
  const nodes = Array.isArray(value.nodes)
    ? value.nodes.filter((node): node is string => typeof node === 'string')
    : [];
  return { kind, nodes };
}

/**
 * Normalized result of a `POST /workflows/start`. The server returns plain UUID
 * strings; this is the camelCase shape the UI consumes. Provenance is honest:
 * these ids exist only because the server confirmed the run was created.
 */
export type StartWorkflowResult = {
  workflowId: WorkflowId;
  runId: string;
};

type RawStartWorkflowResponse = {
  workflow_id?: unknown;
  run_id?: unknown;
};

/**
 * Normalized result of a `POST /deploy/packages` upload
 * (`ProtoLoadPackageResponse`). `freshlyLoaded === false` means the content hash
 * was already resident (idempotent re-upload); `routeChanged` reports whether
 * the call re-pointed the active route.
 */
export type LoadPackageResult = {
  workflowType: string;
  contentHash: string;
  deployedEntryModule: string;
  entryFunction: string;
  freshlyLoaded: boolean;
  routeChanged: boolean;
};

type RawLoadPackageResponse = {
  workflow_type?: unknown;
  content_hash?: unknown;
  deployed_entry_module?: unknown;
  entry_function?: unknown;
  freshly_loaded?: unknown;
  route_changed?: unknown;
};

/** Normalized listing row from `GET /deploy/versions` (`ProtoWorkflowVersion`). */
export type WorkflowVersion = {
  workflowType: string;
  contentHash: string;
  deployedEntryModule: string;
  entryFunction: string;
  manifestVersion: string;
  loadedAt: string;
  routeActive: boolean;
};

type RawWorkflowVersion = {
  workflow_type?: unknown;
  content_hash?: unknown;
  deployed_entry_module?: unknown;
  entry_function?: unknown;
  manifest_version?: unknown;
  loaded_at?: unknown;
  route_active?: unknown;
};

export type ListVersionsResponse = RawWorkflowVersion[] | { versions?: RawWorkflowVersion[] };

type RawEventSearchResult = {
  event?: unknown;
  workflow_id?: WorkflowId;
  seq?: number;
};

export type EventSearchResponse =
  | RawEventSearchResult[]
  | {
      results?: RawEventSearchResult[];
      items?: RawEventSearchResult[];
      next_cursor?: string | null;
      has_more?: boolean;
    };

export function normalizeWorkflowPage(
  response: WorkflowQueryResponse,
  requestedLimit: number
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

export function normalizeHistory(response: HistoryResponse): Event[] {
  const events = Array.isArray(response)
    ? response
    : (response.events ?? readEnvelopeArray<Event>(response.history ?? []));

  return ([...events] as Event[]).sort(
    (left, right) => left.data.envelope.seq - right.data.envelope.seq
  );
}

export function normalizeEventSearch(
  response: EventSearchResponse,
  requestedLimit: number
): WorkflowPage<EventSearchResult> {
  const rawList = Array.isArray(response) ? response : (response.results ?? response.items ?? []);

  const items = rawList.map(toEventSearchResult);

  if (Array.isArray(response)) {
    return { items, nextCursor: null, hasMore: items.length >= requestedLimit };
  }

  return {
    items,
    nextCursor: response.next_cursor ?? null,
    hasMore: response.has_more ?? response.next_cursor != null,
  };
}

export function normalizeNamespaces(response: NamespacesResponse): Namespace[] {
  return Array.isArray(response) ? response : readArray<Namespace>(response, NAMESPACES_KEY);
}

/**
 * Result of an explicit `POST /namespaces` create. Mirrors the server's
 * `CreateNamespaceResponse` (`{ name, created }`): `name` is the durable
 * namespace, `created === true` means THIS call minted the record and
 * `created === false` is the idempotent re-create path (the name already
 * existed). The distinction is surfaced honestly, not collapsed.
 */
export type CreateNamespaceResult = {
  name: Namespace;
  created: boolean;
};

type RawCreateNamespaceResponse = {
  name?: unknown;
  created?: unknown;
};

/**
 * Normalize the `POST /namespaces` response into {@link CreateNamespaceResult}. A
 * missing/non-string `name` is a contract violation surfaced as an {@link ApiError}
 * (never a silent success); a missing/non-boolean `created` collapses to `false`
 * (the conservative "already existed" reading) rather than throwing.
 */
export function normalizeCreateNamespace(response: unknown): CreateNamespaceResult {
  if (!isRecord(response)) {
    throw new ApiError(200, 'namespaces create response was not an object');
  }

  const raw = response as RawCreateNamespaceResponse;

  return {
    name: requireString(raw.name, 'name') as Namespace,
    created: raw.created === true,
  };
}

export function normalizeStartWorkflow(response: unknown): StartWorkflowResult {
  if (!isRecord(response)) {
    throw new ApiError(200, 'workflows/start response was not an object');
  }

  const raw = response as RawStartWorkflowResponse;
  const workflowId = requireString(raw.workflow_id, 'workflow_id');
  const runId = requireString(raw.run_id, 'run_id');

  return { workflowId: workflowId as WorkflowId, runId };
}

export function normalizeLoadPackage(response: unknown): LoadPackageResult {
  if (!isRecord(response)) {
    throw new ApiError(200, 'deploy/packages response was not an object');
  }

  const raw = response as RawLoadPackageResponse;

  return {
    workflowType: requireString(raw.workflow_type, 'workflow_type'),
    contentHash: requireString(raw.content_hash, 'content_hash'),
    deployedEntryModule: asString(raw.deployed_entry_module),
    entryFunction: asString(raw.entry_function),
    freshlyLoaded: raw.freshly_loaded === true,
    routeChanged: raw.route_changed === true,
  };
}

export function normalizeWorkflowVersions(response: ListVersionsResponse): WorkflowVersion[] {
  const rows = Array.isArray(response) ? response : (response.versions ?? []);

  return rows.map(toWorkflowVersion);
}

function toWorkflowVersion(raw: RawWorkflowVersion): WorkflowVersion {
  return {
    workflowType: asString(raw.workflow_type),
    contentHash: asString(raw.content_hash),
    deployedEntryModule: asString(raw.deployed_entry_module),
    entryFunction: asString(raw.entry_function),
    manifestVersion: asString(raw.manifest_version),
    loadedAt: asString(raw.loaded_at),
    routeActive: raw.route_active === true,
  };
}

function requireString(value: unknown, field: string): string {
  if (typeof value !== 'string' || value.length === 0) {
    throw new ApiError(200, `response field "${field}" was missing or not a string`);
  }

  return value;
}

function asString(value: unknown): string {
  return typeof value === 'string' ? value : '';
}

function toEventSearchResult(raw: RawEventSearchResult): EventSearchResult {
  const event = readEnvelopePayload<Event>(raw.event);
  const seq = raw.seq ?? event.data.envelope.seq;
  const workflowId = raw.workflow_id ?? event.data.envelope.workflow_id;

  return { event, seq, workflowId };
}

export function readEnvelopePayload<T>(value: unknown): T {
  if (!isRecord(value)) {
    return value as T;
  }

  const payload = value[PAYLOAD_KEY];

  if (!isRecord(payload)) {
    return value as T;
  }

  const bytes = payload[PAYLOAD_BYTES_KEY];

  if (Array.isArray(bytes)) {
    return JSON.parse(String.fromCharCode(...bytes)) as T;
  }

  if (typeof bytes === 'string') {
    return JSON.parse(decodeBase64(bytes)) as T;
  }

  return value as T;
}

function readEnvelopeArray<T>(values: unknown[]): T[] {
  return values.map((value) => readEnvelopePayload<T>(value));
}

function readArray<T>(record: JsonRecord, key: string): T[] {
  const value = record[key];
  return Array.isArray(value) ? (value as T[]) : [];
}

export async function readJson(response: Response): Promise<unknown> {
  const text = await response.text();

  if (text.length === 0) {
    return null;
  }

  return JSON.parse(text) as unknown;
}

export function apiErrorFromResponse(status: number, body: unknown): ApiError {
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

export function isRecord(value: unknown): value is JsonRecord {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function decodeBase64(value: string): string {
  if (typeof atob === 'function') {
    return atob(value);
  }

  return Buffer.from(value, 'base64').toString('utf8');
}
