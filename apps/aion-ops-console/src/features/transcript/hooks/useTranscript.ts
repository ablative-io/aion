import { useEffect, useMemo, useRef, useState } from 'react';

import type { AionSocketError, ConnectionStatus } from '@/lib/api';
import type { AionTranscriptStreamManager, TranscriptTarget } from '@/lib/api/transcript-stream';
import { createConfiguredTranscriptReader, createConfiguredTranscriptStream } from '@/lib/config';
import type { ActivityEvent, Namespace, WorkflowId } from '@/types';

import type { TranscriptEntry } from '../lib/entries';
import { startTranscriptSession, type TranscriptBackfillState } from './transcriptSession';

/**
 * Live transcript hook (NOI-7).
 *
 * Subscribes to the dedicated transcript WebSocket for ONE
 * `(namespace, workflow, activity, attempt)` target and exposes the ordered,
 * deduplicated, COALESCED {@link TranscriptEntry} stream. REAL DATA ONLY,
 * socket-first, NO polling: the durable retained transcript is fetched over REST
 * first (lane #229), then the live WS tail attaches with `after_seq` at the
 * backfill's resume cursor, and the manager resumes by `store_seq` on reconnect
 * so there is no gap or duplicate.
 *
 * The sequencing itself lives in {@link startTranscriptSession} — a plain async
 * controller proven by direct execution in `transcriptSession.test.ts` — so this
 * hook is only the React adapter: state cells, target-change reset, and the
 * effect that starts/stops one session per target.
 *
 * The entry model + fold rules (dedup, `store_seq` ordering, delta/note
 * coalescing) live in `../lib/entries` and are re-exported here for callers.
 */

export type {
  TranscriptEntry,
  TranscriptEventEntry,
  TranscriptNoteRunEntry,
  TranscriptStreamEntry,
  TranscriptToolEntry,
  TranscriptToolStreamEntry,
} from '../lib/entries';
export { backfillEntries, foldTranscriptEvent } from '../lib/entries';

export type UseTranscriptOptions = {
  /** The attempt target to subscribe to; `null` disables the subscription. */
  target: TranscriptTarget | null;
  /**
   * Override the stream manager factory (tests); defaults to the configured
   * one. `initialAfterSeq` seeds the WS resume cursor from the REST backfill.
   */
  createManager?: (
    target: TranscriptTarget,
    initialAfterSeq?: number
  ) => AionTranscriptStreamManager;
  /** Override the retained-transcript REST fetch (tests); defaults configured. */
  fetchRetained?: (target: TranscriptTarget) => Promise<ActivityEvent[]>;
};

export type UseTranscriptResult = {
  /** The ordered, deduplicated transcript entries (durable tail + live deltas). */
  entries: TranscriptEntry[];
  /** Live socket connection status. */
  status: ConnectionStatus;
  /** Typed live-socket error, or null (no silent failure). */
  socketError: AionSocketError | null;
  /** REST-backfill lifecycle: the retained history fetch that precedes the WS. */
  backfillState: TranscriptBackfillState;
  /** The retained-fetch failure, surfaced visibly (the WS replay still runs). */
  backfillError: Error | null;
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

/** Default retained-transcript fetch: the configured lane-#229 REST client. */
async function defaultFetchRetained(target: TranscriptTarget): Promise<ActivityEvent[]> {
  const window = await createConfiguredTranscriptReader({
    namespace: target.namespace,
  }).fetchTranscript(target);
  return window.events;
}

/** Default manager factory: the configured WS stream, cursor-seeded. */
function defaultCreateManager(
  target: TranscriptTarget,
  initialAfterSeq?: number
): AionTranscriptStreamManager {
  return createConfiguredTranscriptStream(target, undefined, initialAfterSeq);
}

export function useTranscript(options: UseTranscriptOptions): UseTranscriptResult {
  const { target } = options;
  const createManager = options.createManager ?? defaultCreateManager;
  const fetchRetained = options.fetchRetained ?? defaultFetchRetained;
  const [entries, setEntries] = useState<TranscriptEntry[]>([]);
  const [status, setStatus] = useState<ConnectionStatus>('disconnected');
  const [socketError, setSocketError] = useState<AionSocketError | null>(null);
  const [backfillState, setBackfillState] = useState<TranscriptBackfillState>('ready');
  const [backfillError, setBackfillError] = useState<Error | null>(null);
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
      setBackfillState('ready');
      setBackfillError(null);
      return;
    }
    // `key !== ''` iff `target` is non-null; the parse recovers the exact fields.
    const resolved = parseTargetKey(key);

    // Reset on target change: a new attempt is a fresh transcript.
    setEntries([]);
    deltaCounter.current = 0;
    setBackfillError(null);
    setBackfillState('loading');

    const session = startTranscriptSession({
      createManager,
      deltaCounter,
      fetchRetained,
      onBackfill: (state, error) => {
        setBackfillError(error);
        setBackfillState(state);
      },
      onEntries: setEntries,
      onSocketError: setSocketError,
      onStatus: setStatus,
      target: resolved,
    });

    return session.stop;
  }, [key, createManager, fetchRetained]);

  return useMemo(
    () => ({ entries, status, socketError, backfillState, backfillError }),
    [entries, status, socketError, backfillState, backfillError]
  );
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
