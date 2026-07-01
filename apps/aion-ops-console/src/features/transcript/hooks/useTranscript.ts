import { useEffect, useMemo, useRef, useState } from 'react';

import type { AionSocketError, ConnectionStatus } from '@/lib/api';
import type {
  AionTranscriptStreamManager,
  TranscriptStreamListener,
  TranscriptTarget,
} from '@/lib/api/transcript-stream';
import { createConfiguredTranscriptStream } from '@/lib/config';
import type { ActivityEvent, Namespace, WorkflowId } from '@/types';

/**
 * Live transcript hook (NOI-7).
 *
 * Subscribes to the dedicated transcript WebSocket for ONE
 * `(namespace, workflow, activity, attempt)` target and exposes the ordered,
 * deduplicated {@link ActivityEvent} stream. REAL DATA ONLY, socket-first, NO
 * polling: the durable `O`-keyspace tail arrives first, then live deltas splice on,
 * and the manager resumes by `store_seq` on reconnect so there is no gap or
 * duplicate.
 *
 * Dedup + order rules (mirroring the server splice contract):
 *
 * - A PERSISTED event carries a numeric `store_seq`. Two persisted events with the
 *   same `store_seq` are the SAME event (a reconnect race re-delivered one); the
 *   later arrival is dropped. Persisted events are kept ordered by `store_seq`.
 * - An EPHEMERAL token delta carries `store_seq: null`. It is never persisted and
 *   never replayed, so it is appended in arrival order and is inherently unique
 *   (a fresh delta each time); it is retained after the last persisted event.
 */

/** A rendered transcript entry: the event plus a stable React key. */
export type TranscriptEntry = {
  /** Stable key: the store_seq for persisted events, or a monotonic id for deltas. */
  key: string;
  event: ActivityEvent;
};

export type UseTranscriptOptions = {
  /** The attempt target to subscribe to; `null` disables the subscription. */
  target: TranscriptTarget | null;
  /** Override the stream manager factory (tests); defaults to the configured one. */
  createManager?: (target: TranscriptTarget) => AionTranscriptStreamManager;
};

export type UseTranscriptResult = {
  /** The ordered, deduplicated transcript entries (durable tail + live deltas). */
  entries: TranscriptEntry[];
  /** Live socket connection status. */
  status: ConnectionStatus;
  /** Typed live-socket error, or null (no silent failure). */
  socketError: AionSocketError | null;
};

/** A serialized target identity for effect dependency + remount on target change. */
export function targetKey(target: TranscriptTarget | null): string {
  if (target === null) {
    return '';
  }
  return [
    target.namespace,
    target.workflowId,
    String(target.activityId),
    String(target.attempt),
  ].join('\0');
}

/**
 * Fold one incoming event into the ordered, deduplicated entry list. A persisted
 * event (numeric `store_seq`) is inserted in `store_seq` order and deduplicated on
 * that sequence; an ephemeral delta is appended in arrival order after the last
 * persisted event, keyed by a caller-supplied monotonic id so React keys stay
 * stable and unique. Returns the SAME array reference when a persisted duplicate is
 * dropped, so React can skip the re-render.
 */
export function foldTranscriptEvent(
  entries: TranscriptEntry[],
  event: ActivityEvent,
  deltaId: number
): TranscriptEntry[] {
  const storeSeq = event.store_seq;
  if (typeof storeSeq !== 'number') {
    // Ephemeral delta: unique by arrival, appended at the tail.
    return [...entries, { key: `delta:${deltaId}`, event }];
  }

  const key = `seq:${storeSeq}`;
  if (entries.some((entry) => entry.key === key)) {
    // A reconnect race re-delivered this persisted event: drop the duplicate.
    return entries;
  }

  // Insert in store_seq order among the persisted entries. `Array.prototype.sort`
  // is stable, so ephemeral deltas (no store_seq) keep their arrival order and any
  // persisted-vs-ephemeral pair is left in arrival order (comparator returns 0).
  const next = [...entries, { key, event }];
  next.sort(comparePersistedThenArrival);
  return next;
}

/** Order persisted entries by store_seq; any pair lacking one keeps arrival order. */
function comparePersistedThenArrival(left: TranscriptEntry, right: TranscriptEntry): number {
  const leftSeq = left.event.store_seq;
  const rightSeq = right.event.store_seq;
  if (typeof leftSeq === 'number' && typeof rightSeq === 'number') {
    return leftSeq - rightSeq;
  }
  return 0;
}

export function useTranscript(options: UseTranscriptOptions): UseTranscriptResult {
  const { target } = options;
  const createManager = options.createManager ?? createConfiguredTranscriptStream;
  const [entries, setEntries] = useState<TranscriptEntry[]>([]);
  const [status, setStatus] = useState<ConnectionStatus>('disconnected');
  const [socketError, setSocketError] = useState<AionSocketError | null>(null);
  // A monotonic id source for ephemeral-delta React keys, stable across renders.
  const deltaCounter = useRef(0);
  // Serialize the target so the subscribe effect re-runs on ANY field change
  // while depending on a single primitive (a fresh attempt is a fresh transcript).
  const key = targetKey(target);

  useEffect(() => {
    if (key === '') {
      setEntries([]);
      setStatus('disconnected');
      setSocketError(null);
      return;
    }
    // `key !== ''` iff `target` is non-null; the parse recovers the exact fields.
    const resolved = parseTargetKey(key);

    // Reset on target change: a new attempt is a fresh transcript.
    setEntries([]);
    deltaCounter.current = 0;

    const manager = createManager(resolved);

    const listener: TranscriptStreamListener = {
      onEvent: (event) => {
        const deltaId = deltaCounter.current;
        deltaCounter.current += 1;
        setEntries((current) => foldTranscriptEvent(current, event, deltaId));
      },
    };

    const unsubscribeStream = manager.subscribe(listener);
    const unsubscribeStatus = manager.onStatusChange(setStatus);
    const unsubscribeError = manager.onError(setSocketError);
    setStatus(manager.getStatus());

    return () => {
      unsubscribeStream();
      unsubscribeStatus();
      unsubscribeError();
    };
  }, [key, createManager]);

  return useMemo(() => ({ entries, status, socketError }), [entries, status, socketError]);
}

/** Recover a {@link TranscriptTarget} from its serialized {@link targetKey}. */
export function parseTargetKey(key: string): TranscriptTarget {
  const [namespace, workflowId, activityId, attempt] = key.split('\0');
  return {
    namespace: (namespace ?? '') as Namespace,
    workflowId: (workflowId ?? '') as WorkflowId,
    activityId: Number(activityId),
    attempt: Number(attempt),
  };
}
