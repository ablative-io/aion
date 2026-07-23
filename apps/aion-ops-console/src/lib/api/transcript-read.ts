import type { ActivityEvent, ActivityId, Namespace, WorkflowId } from '@/types';

import { ApiError } from './api-error';
import { apiErrorFromResponse, readJson } from './client-normalize';
import {
  type ApiCredentials,
  buildScopedHeaders,
  buildUrl,
  stripTrailingSlash,
} from './client-transport';
import type { TranscriptTarget } from './transcript-stream';

/**
 * Durable transcript READ client (lane #230): the console consumption of the
 * lane-#229 REST pair. `POST /workflows/transcript` fetches one retained
 * `(workflow, activity, attempt)` stream in `store_seq` order;
 * `POST /workflows/transcripts` enumerates a workflow's retained streams.
 *
 * The documented contract: REST fetch FIRST, then attach the transcript WS with
 * `after_seq` = the last fetched `store_seq`, so the socket serves only the live
 * tail past the fetched history (the server suppresses `store_seq <= after_seq`
 * and the console fold also drops persisted duplicates — the splice seam is
 * double-covered). An empty list/array is the honest answer for a pre-retention
 * run, never an error. Kept out of `client.ts` deliberately: that file is
 * already at the house LOC limit.
 */

/** The lane-#229 REST endpoints (server routes in `transcripts.rs`). */
export const TRANSCRIPT_READ = {
  fetch: '/workflows/transcript',
  streams: '/workflows/transcripts',
} as const;

type FetchFn = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

export type TranscriptReadOptions = {
  baseUrl?: string;
  fetchImpl?: FetchFn;
  credentials?: ApiCredentials;
};

/** `last` and `from_seq` are mutually exclusive on the wire. */
export type TranscriptFetchParams = TranscriptTarget &
  (
    | { fromSeq?: number; last?: never; limit?: number }
    | { fromSeq?: never; last: number; limit?: number }
  );

export type TranscriptWindow = {
  events: ActivityEvent[];
  /** Store sequence to resume from when the requested limit truncated the read. */
  nextFromSeq: number | null;
  /** Retained stream head at read time. */
  headSeq: number;
};

/** One retained stream head from the enumeration endpoint. */
export type RetainedStreamHead = {
  activityId: ActivityId;
  attempt: number;
  /** Next `store_seq` to be written == count of retained records. */
  head: number;
};

export class TranscriptReadClient {
  private readonly baseUrl: string;
  private readonly fetchImpl: FetchFn;
  private readonly credentials: ApiCredentials | undefined;

  constructor(options: TranscriptReadOptions = {}) {
    this.baseUrl = stripTrailingSlash(options.baseUrl ?? '');
    // The default must close over the global fetch, never store the bare
    // reference: `this.fetchImpl(...)` would invoke fetch with `this` set to
    // the client and the browser throws "Illegal invocation" (same trap
    // ApiClient documents and avoids).
    this.fetchImpl = options.fetchImpl ?? ((input, init) => globalThis.fetch(input, init));
    this.credentials = options.credentials;
  }

  /**
   * Fetch the retained transcript of one stream, in `store_seq` order. Events
   * are returned verbatim alongside the always-present window cursors. An empty
   * events array is the honest pre-retention answer.
   */
  async fetchTranscript(params: TranscriptFetchParams): Promise<TranscriptWindow> {
    const body = {
      namespace: params.namespace,
      workflow_id: params.workflowId,
      activity_id: params.activityId,
      attempt: params.attempt,
      ...(params.fromSeq === undefined ? {} : { from_seq: params.fromSeq }),
      ...(params.limit === undefined ? {} : { limit: params.limit }),
      ...(params.last === undefined ? {} : { last: params.last }),
    };
    const responseBody = await this.post(TRANSCRIPT_READ.fetch, body);
    const responseRecord = isRecord(responseBody) ? responseBody : null;
    const events = responseRecord?.events;
    if (responseRecord === null || !Array.isArray(events)) {
      throw new ApiError(200, 'workflows/transcript response missing an events array');
    }
    const nextFromSeq = responseRecord.next_from_seq;
    const headSeq = responseRecord.head_seq;
    if (nextFromSeq !== null && !isSequence(nextFromSeq)) {
      throw new ApiError(200, 'workflows/transcript response has an invalid next_from_seq');
    }
    if (!isSequence(headSeq)) {
      throw new ApiError(200, 'workflows/transcript response has an invalid head_seq');
    }
    return {
      events: events as ActivityEvent[],
      nextFromSeq,
      headSeq,
    };
  }

  /** Enumerate a workflow's retained transcript streams (may be empty). */
  async listStreams(namespace: Namespace, workflowId: WorkflowId): Promise<RetainedStreamHead[]> {
    const responseBody = await this.post(TRANSCRIPT_READ.streams, {
      namespace,
      workflow_id: workflowId,
    });
    const streams = isRecord(responseBody) ? responseBody.streams : undefined;
    if (!Array.isArray(streams)) {
      throw new ApiError(200, 'workflows/transcripts response missing a streams array');
    }
    // A malformed row THROWS (truth-first): silently skipping would hide a
    // contract drift behind a shorter badge list.
    return streams.map(readStreamHead);
  }

  private async post(path: string, body: Record<string, unknown>): Promise<unknown> {
    const response = await this.fetchImpl(buildUrl(this.baseUrl, path), {
      method: 'POST',
      headers: buildScopedHeaders(this.credentials),
      body: JSON.stringify(body),
    });

    if (!response.ok) {
      const errorBody = await readJson(response).catch(() => null);
      throw apiErrorFromResponse(response.status, errorBody);
    }

    return readJson(response);
  }
}

function readStreamHead(row: unknown): RetainedStreamHead {
  if (!isRecord(row)) {
    throw new ApiError(200, 'workflows/transcripts stream row is not an object');
  }
  const activityId = row.activity_id;
  const attempt = row.attempt;
  const head = row.head;
  if (typeof activityId !== 'number' || typeof attempt !== 'number' || typeof head !== 'number') {
    throw new ApiError(200, 'workflows/transcripts stream row missing activity_id/attempt/head');
  }
  return { activityId, attempt, head };
}

function isSequence(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
