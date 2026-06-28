import type { Event, Namespace, WorkflowId, WorkflowSummary } from '@/types';

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
