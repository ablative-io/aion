import { describe, expect, test } from 'bun:test';

import type { AionSocketError, ConnectionStatus } from '@/lib/api';
import type {
  AionTranscriptStreamManager,
  TranscriptStreamListener,
  TranscriptTarget,
} from '@/lib/api/transcript-stream';
import type { ActivityEvent, Namespace, WorkflowId } from '@/types';

import {
  startTranscriptSession,
  type TranscriptBackfillState,
  type TranscriptSessionOptions,
} from '../hooks/transcriptSession';
import type { TranscriptEntry } from '../lib/entries';

/**
 * DIRECT EXECUTION proof of the REST-backfill-first-then-WS-tail sequencing
 * (the work-package-C headline): these tests drive the extracted controller
 * through its injected seams — the exact seams `useTranscript` wires — and
 * assert the ordering, the resume cursor, the error fallback, the cancellation
 * guard, and the teardown, none of which an SSR render can execute.
 */

const TARGET: TranscriptTarget = {
  namespace: 'default' as Namespace,
  workflowId: '00000000-0000-0000-0000-000000000001' as WorkflowId,
  activityId: 3,
  attempt: 1,
};

function persistedMessage(storeSeq: number, text: string): ActivityEvent {
  return {
    workflow_id: TARGET.workflowId,
    activity_id: TARGET.activityId,
    attempt: TARGET.attempt,
    agent_id: '00000000-0000-0000-0000-0000000000aa',
    agent_role: 'orchestrator',
    emitted_at: '2026-07-01T00:00:00Z',
    worker_seq: storeSeq,
    store_seq: storeSeq,
    ephemeral: false,
    kind: { kind: 'Message', role: { role: 'Assistant' }, text },
  };
}

/** A fake WS manager recording its wiring; `emit` drives the live tail. */
class FakeManager {
  readonly initialAfterSeq: number | undefined;
  readonly unsubscribed: string[] = [];
  private listener: TranscriptStreamListener | null = null;

  constructor(initialAfterSeq: number | undefined) {
    this.initialAfterSeq = initialAfterSeq;
  }

  subscribe(listener: TranscriptStreamListener) {
    this.listener = listener;
    return () => {
      this.unsubscribed.push('stream');
    };
  }

  onStatusChange(_listener: (status: ConnectionStatus) => void) {
    return () => {
      this.unsubscribed.push('status');
    };
  }

  onError(_listener: (error: AionSocketError | null) => void) {
    return () => {
      this.unsubscribed.push('error');
    };
  }

  getStatus(): ConnectionStatus {
    return 'connected';
  }

  emit(event: ActivityEvent) {
    this.listener?.onEvent(event);
  }
}

/** One fully-instrumented controller run; every sink records its calls. */
function harness(fetchRetained: (target: TranscriptTarget) => Promise<ActivityEvent[]>) {
  const managers: FakeManager[] = [];
  let entries: TranscriptEntry[] = [];
  const entryUpdates: TranscriptEntry[][] = [];
  const statuses: ConnectionStatus[] = [];
  const backfills: { state: TranscriptBackfillState; error: Error | null }[] = [];
  const deltaCounter = { current: 0 };
  const fetchedTargets: TranscriptTarget[] = [];
  // How many entry updates had landed when each manager was created — the
  // direct order proof that the backfill strictly precedes the attach.
  const entryUpdatesAtAttach: number[] = [];

  const options: TranscriptSessionOptions = {
    createManager: (target, initialAfterSeq) => {
      expect(target).toEqual(TARGET);
      const manager = new FakeManager(initialAfterSeq);
      managers.push(manager);
      entryUpdatesAtAttach.push(entryUpdates.length);
      return manager as unknown as AionTranscriptStreamManager;
    },
    deltaCounter,
    fetchRetained: (target) => {
      fetchedTargets.push(target);
      return fetchRetained(target);
    },
    onBackfill: (state, error) => {
      backfills.push({ state, error });
    },
    onEntries: (update) => {
      entries = update(entries);
      entryUpdates.push(entries);
    },
    onSocketError: () => {},
    onStatus: (status) => {
      statuses.push(status);
    },
    target: TARGET,
  };

  return {
    backfills,
    deltaCounter,
    entryUpdates,
    entryUpdatesAtAttach,
    fetchedTargets,
    getEntries: () => entries,
    managers,
    options,
    statuses,
  };
}

describe('startTranscriptSession', () => {
  test('backfill lands BEFORE the WS attaches, and the tail resumes at lastSeq', async () => {
    const retained = [persistedMessage(4, 'first'), persistedMessage(7, 'second')];
    const h = harness(() => Promise.resolve(retained));

    const session = startTranscriptSession(h.options);
    // Nothing attaches synchronously: the REST backfill strictly precedes the WS.
    expect(h.managers).toHaveLength(0);
    await session.settled;

    // The retained history was fetched for the session's exact target.
    expect(h.fetchedTargets).toEqual([TARGET]);
    // The folded backfill replaced the entry list BEFORE the manager existed:
    // the first update carries both retained events, and exactly ONE entry
    // update had landed at the moment the manager was created.
    expect(h.entryUpdates[0]?.map((entry) => entry.key)).toEqual(['seq:4', 'seq:7']);
    expect(h.entryUpdatesAtAttach).toEqual([1]);
    expect(h.backfills).toEqual([{ state: 'ready', error: null }]);
    // EXACTLY one manager, seeded with the resume cursor = highest store_seq.
    expect(h.managers).toHaveLength(1);
    expect(h.managers[0]?.initialAfterSeq).toBe(7);
    // The manager's current status was pushed on attach.
    expect(h.statuses).toEqual(['connected']);
    session.stop();
  });

  test('an empty retained transcript attaches with NO cursor (full WS replay)', async () => {
    const h = harness(() => Promise.resolve([]));

    const session = startTranscriptSession(h.options);
    await session.settled;

    expect(h.managers).toHaveLength(1);
    expect(h.managers[0]?.initialAfterSeq).toBeUndefined();
    expect(h.backfills).toEqual([{ state: 'ready', error: null }]);
    session.stop();
  });

  test('live events fold onto the backfill with keys past the consumed ids', async () => {
    const retained = [persistedMessage(4, 'first'), persistedMessage(7, 'second')];
    const h = harness(() => Promise.resolve(retained));

    const session = startTranscriptSession(h.options);
    await session.settled;

    // The delta counter advanced past the backfill's consumed ids.
    expect(h.deltaCounter.current).toBe(2);

    // A live tail event folds onto the retained entries in store_seq order.
    h.managers[0]?.emit(persistedMessage(9, 'tail'));
    expect(h.getEntries().map((entry) => entry.key)).toEqual(['seq:4', 'seq:7', 'seq:9']);
    // A replayed duplicate of a retained event is dropped (same list back).
    h.managers[0]?.emit(persistedMessage(7, 'second'));
    expect(h.getEntries().map((entry) => entry.key)).toEqual(['seq:4', 'seq:7', 'seq:9']);
    session.stop();
  });

  test('a backfill failure is surfaced visibly and the WS still attaches (full replay)', async () => {
    const failure = new Error('retention endpoint unavailable');
    const h = harness(() => Promise.reject(failure));

    const session = startTranscriptSession(h.options);
    await session.settled;

    expect(h.backfills).toEqual([{ state: 'error', error: failure }]);
    // No entries were fabricated from the failed fetch.
    expect(h.entryUpdates).toHaveLength(0);
    // The WS fallback attached with NO cursor: the durable replay serves everything.
    expect(h.managers).toHaveLength(1);
    expect(h.managers[0]?.initialAfterSeq).toBeUndefined();
    session.stop();
  });

  test('stop() before the fetch settles cancels: no state, no attach', async () => {
    let resolveFetch: (events: ActivityEvent[]) => void = () => {};
    const pending = new Promise<ActivityEvent[]>((resolve) => {
      resolveFetch = resolve;
    });
    const h = harness(() => pending);

    const session = startTranscriptSession(h.options);
    session.stop();
    resolveFetch([persistedMessage(4, 'late')]);
    await session.settled;

    expect(h.entryUpdates).toHaveLength(0);
    expect(h.backfills).toHaveLength(0);
    expect(h.managers).toHaveLength(0);
  });

  test('stop() before a fetch FAILURE settles suppresses the error path too', async () => {
    let rejectFetch: (error: Error) => void = () => {};
    const pending = new Promise<ActivityEvent[]>((_resolve, reject) => {
      rejectFetch = reject;
    });
    const h = harness(() => pending);

    const session = startTranscriptSession(h.options);
    session.stop();
    rejectFetch(new Error('late failure'));
    await session.settled;

    expect(h.backfills).toHaveLength(0);
    expect(h.managers).toHaveLength(0);
  });

  test('stop() after attach unsubscribes stream, status, AND error listeners', async () => {
    const h = harness(() => Promise.resolve([persistedMessage(4, 'first')]));

    const session = startTranscriptSession(h.options);
    await session.settled;
    expect(h.managers[0]?.unsubscribed).toEqual([]);

    session.stop();
    expect(h.managers[0]?.unsubscribed.toSorted()).toEqual(['error', 'status', 'stream']);
  });
});
