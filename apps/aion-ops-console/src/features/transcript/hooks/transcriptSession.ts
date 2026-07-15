import type { AionSocketError, ConnectionStatus } from '@/lib/api';
import type {
  AionTranscriptStreamManager,
  TranscriptStreamListener,
  TranscriptTarget,
} from '@/lib/api/transcript-stream';
import type { ActivityEvent } from '@/types';

import { backfillEntries, foldTranscriptEvent, type TranscriptEntry } from '../lib/entries';

/**
 * The transcript session controller: the REST-backfill-first-then-WS-tail
 * sequencing extracted from {@link useTranscript} into a plain async function so
 * its runtime behavior is provable by DIRECT EXECUTION in tests (the SSR-only
 * suite never runs effects). The hook's effect is a thin adapter: it starts a
 * session wired to its `setState` sinks and returns {@link TranscriptSession.stop}
 * as the cleanup.
 *
 * Contract (the lane's headline sequencing):
 *
 * 1. `fetchRetained(target)` loads the durable retained transcript over REST.
 * 2. On success the folded backfill replaces the entry list, the delta counter is
 *    advanced past the consumed ids, backfill reports `ready`, and the WS tail
 *    attaches with `after_seq = lastSeq` (or a full replay when the retained
 *    transcript was empty — a pre-retention run, the honest fallback).
 * 3. On failure the error is surfaced VISIBLY (`error` state, never swallowed)
 *    and the WS still attaches with a full durable replay.
 * 4. `stop()` cancels: a fetch settling after `stop()` neither mutates state nor
 *    attaches, and an attached manager's listeners are all unsubscribed.
 */

export type TranscriptBackfillState = 'loading' | 'ready' | 'error';

export type TranscriptSessionOptions = {
  /** The attempt target the session serves. */
  target: TranscriptTarget;
  /** WS manager factory; `initialAfterSeq` seeds the resume cursor. */
  createManager: (
    target: TranscriptTarget,
    initialAfterSeq?: number
  ) => AionTranscriptStreamManager;
  /** The retained-transcript REST fetch preceding the WS attach. */
  fetchRetained: (target: TranscriptTarget) => Promise<ActivityEvent[]>;
  /**
   * Monotonic id source for ephemeral-delta keys, owned by the caller so ids
   * stay unique across re-subscribes of the same mounted hook.
   */
  deltaCounter: { current: number };
  /** Entry-list sink (updater form, mirroring React's functional `setState`). */
  onEntries: (update: (current: TranscriptEntry[]) => TranscriptEntry[]) => void;
  /** Live socket status sink. */
  onStatus: (status: ConnectionStatus) => void;
  /** Typed live-socket error sink. */
  onSocketError: (error: AionSocketError | null) => void;
  /** Backfill lifecycle sink (`ready`/`error` + the visible failure, if any). */
  onBackfill: (state: TranscriptBackfillState, error: Error | null) => void;
};

export type TranscriptSession = {
  /**
   * Resolves once the backfill has settled AND the WS tail attached (or the
   * session was stopped first). Never rejects: the fetch failure is routed to
   * `onBackfill('error', …)`.
   */
  settled: Promise<void>;
  /** Cancel the session: suppress a late fetch and tear down the WS listeners. */
  stop: () => void;
};

export function startTranscriptSession(options: TranscriptSessionOptions): TranscriptSession {
  const { target, createManager, fetchRetained, deltaCounter } = options;

  let cancelled = false;
  let teardown: (() => void) | null = null;

  // Attach the live WS tail (after the REST backfill resolves either way).
  const attach = (initialAfterSeq: number | undefined) => {
    const manager = createManager(target, initialAfterSeq);

    const listener: TranscriptStreamListener = {
      onEvent: (event) => {
        const deltaId = deltaCounter.current;
        deltaCounter.current += 1;
        options.onEntries((current) => foldTranscriptEvent(current, event, deltaId));
      },
    };

    const unsubscribeStream = manager.subscribe(listener);
    const unsubscribeStatus = manager.onStatusChange(options.onStatus);
    const unsubscribeError = manager.onError(options.onSocketError);
    options.onStatus(manager.getStatus());
    teardown = () => {
      unsubscribeStream();
      unsubscribeStatus();
      unsubscribeError();
    };
  };

  const settled = fetchRetained(target)
    .then((events) => {
      if (cancelled) {
        return;
      }
      const { entries: retained, lastSeq } = backfillEntries(events);
      // Keep live entry keys unique past the backfill's consumed ids.
      deltaCounter.current = events.length;
      options.onEntries(() => retained);
      options.onBackfill('ready', null);
      // REST history + WS tail (after_seq = lastSeq); lastSeq undefined ⇒
      // full WS replay (pre-retention run — the honest fallback).
      attach(lastSeq);
    })
    .catch((error: unknown) => {
      if (cancelled) {
        return;
      }
      options.onBackfill('error', error instanceof Error ? error : new Error(String(error)));
      // Resilient fallback: the WS durable replay still serves the full
      // history; the REST failure stays VISIBLE (never swallowed).
      attach(undefined);
    });

  return {
    settled,
    stop: () => {
      cancelled = true;
      teardown?.();
    },
  };
}
