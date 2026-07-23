import type { Event } from '@/types';

import { ApiError } from './api-error';

/** Optional fields use the server defaults when omitted. */
export type HistoryWindowRequest = {
  fromSeq?: number;
  limit?: number;
  /** Zero disables payload-byte elision. */
  payloadLimitBytes?: number;
};

export type HistoryWindow = {
  events: Event[];
  nextFromSeq: number | null;
  headSeq: number;
};

export function decodeHistoryWindow(value: unknown): HistoryWindow {
  if (!isRecord(value) || !Array.isArray(value.events)) {
    throw contractError('workflows/history response missing an events array');
  }

  const nextFromSeq = value.next_from_seq;
  if (nextFromSeq !== null && !isSequence(nextFromSeq)) {
    throw contractError('workflows/history response has an invalid next_from_seq');
  }
  if (!isSequence(value.head_seq)) {
    throw contractError('workflows/history response has an invalid head_seq');
  }
  if (!value.events.every(isEvent)) {
    throw contractError('workflows/history response contains an invalid event');
  }

  return {
    events: value.events,
    nextFromSeq,
    headSeq: value.head_seq,
  };
}

export function decodeEventResponse(value: unknown): Event {
  if (!isRecord(value) || !isEvent(value.event)) {
    throw contractError('workflows/event response missing a valid event');
  }
  return value.event;
}

function isEvent(value: unknown): value is Event {
  if (!isRecord(value) || typeof value.type !== 'string' || !isRecord(value.data)) {
    return false;
  }
  const envelope = value.data.envelope;
  return (
    isRecord(envelope) &&
    isSequence(envelope.seq) &&
    typeof envelope.recorded_at === 'string' &&
    typeof envelope.workflow_id === 'string'
  );
}

function isSequence(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function contractError(message: string): ApiError {
  return new ApiError(200, message);
}
