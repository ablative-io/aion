import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { eventSequence, mergeEventsBySequence } from '@/features/workflow-detail/lib/timeline';
import type { AionEventWebSocketManager, ApiClient, ConnectionStatus } from '@/lib/api';
import type { FirehoseEventSubscriptionFilter, ResyncContext } from '@/lib/api/websocket';
import { configuredEventSocket, createConfiguredApiClient } from '@/lib/config';
import type { Event, Namespace, WorkflowId, WorkflowStatus } from '@/types';

import { tallyExactlyOnce } from '../lib/exactlyOnce';

/**
 * The fan-out progress + exactly-once data core (DEMO-VIEW-PLAN §2, §4.1 — the riskiest
 * step). Subscribes to the namespace firehose for live events AND back-fills history via
 * `POST /workflows/describe`, then MERGES the two by sequence number. Because the firehose
 * carries no resume cursor, a reconnect re-fetches describe and re-merges by seq, so the
 * count can NEVER double-count: the merged set is seq-deduped (`mergeEventsBySequence`)
 * and the count is derived only from that set (`tallyExactlyOnce`).
 *
 * No silent failures: a describe error is surfaced on `error`; a dropped WS flips
 * `connection` to a visible non-`connected` state and sets `stale`.
 */

const FIREHOSE_SUBSCRIPTION_MANAGER = configuredEventSocket;
const ACTIVITY_SCHEDULED = 'ActivityScheduled';

export type FanOutConnection = ConnectionStatus;

export type FanOutProgress = {
  /** Seq-ordered, deduped merged event set (history + live). */
  events: Event[];
  /** Distinct completed activities (the headline exactly-once count). */
  completedCount: number;
  /** Expected fan-out width, derived from distinct scheduled ordinals (or seed). */
  arity: number | null;
  /** Workflow status derived from terminal lifecycle events, or 'Running'. */
  status: WorkflowStatus;
  /** True when the live stream is not confirmed connected (count may be behind). */
  stale: boolean;
  /** Surfaced error from the describe back-fill; null when healthy. */
  error: string | null;
  /** Force a describe re-fetch + re-merge (used by the resync path and callers). */
  refetch: () => void;
};

type HistoryClient = Pick<ApiClient, 'getHistory'>;
type Manager = Pick<AionEventWebSocketManager, 'subscribe' | 'getStatus' | 'onStatusChange'>;

export type UseFanOutProgressOptions = {
  namespace: Namespace;
  workflowId: WorkflowId | null;
  /** Read target base URL; a change (own-read failover) rebuilds the client. */
  baseUrl?: string | undefined;
  apiClient?: HistoryClient | undefined;
  manager?: Manager | undefined;
  /** Seed arity when no ActivityScheduled events are present yet (e.g. collect_four → 4). */
  seedArity?: number | null | undefined;
  enabled?: boolean | undefined;
};

export function deriveArity(events: readonly Event[], seedArity: number | null): number | null {
  const scheduled = new Set<number>();

  for (const event of events) {
    if (event.type === ACTIVITY_SCHEDULED) {
      scheduled.add(event.data.activity_id);
    }
  }

  if (scheduled.size > 0) {
    return scheduled.size;
  }

  return seedArity;
}

export function deriveStatus(events: readonly Event[]): WorkflowStatus {
  let status: WorkflowStatus = 'Running';

  for (const event of events) {
    switch (event.type) {
      case 'WorkflowCompleted':
        status = 'Completed';
        break;
      case 'WorkflowFailed':
        status = 'Failed';
        break;
      case 'WorkflowCancelled':
        status = 'Cancelled';
        break;
      case 'WorkflowTimedOut':
        status = 'TimedOut';
        break;
      default:
        break;
    }
  }

  return status;
}

export function useFanOutProgress({
  namespace,
  workflowId,
  baseUrl,
  apiClient,
  manager = FIREHOSE_SUBSCRIPTION_MANAGER,
  seedArity = null,
  enabled = true,
}: UseFanOutProgressOptions): FanOutProgress {
  const [historyEvents, setHistoryEvents] = useState<Event[]>([]);
  const [liveEvents, setLiveEvents] = useState<Event[]>([]);
  const [connection, setConnection] = useState<FanOutConnection>('disconnected');
  const [error, setError] = useState<string | null>(null);

  // A new client per baseUrl so own-read failover repoints describe at the survivor.
  // Credentials (x-aion-namespaces) come from config, scoped to this namespace.
  const client = useMemo<HistoryClient>(
    () =>
      apiClient ??
      createConfiguredApiClient({ namespace, ...(baseUrl !== undefined && { baseUrl }) }),
    [apiClient, baseUrl, namespace]
  );

  const liveSeqs = useRef<Set<number>>(new Set());
  const historyRequestGeneration = useRef(0);
  const historyAbort = useRef<AbortController | null>(null);

  const fetchHistory = useCallback(
    async (recovery?: ResyncContext): Promise<void> => {
      if (!enabled || workflowId === null) {
        return;
      }

      historyRequestGeneration.current += 1;
      const generation = historyRequestGeneration.current;
      historyAbort.current?.abort();
      const controller = new AbortController();
      historyAbort.current = controller;
      const abort = () => controller.abort();
      recovery?.signal.addEventListener('abort', abort, { once: true });

      try {
        const result = await requestHistory(
          client,
          workflowId,
          namespace,
          controller,
          generation,
          () => historyRequestGeneration.current,
          recovery
        );
        if (result.kind === 'stale') {
          return;
        }
        if (result.kind === 'failed') {
          const reason =
            result.cause instanceof Error ? result.cause.message : 'describe back-fill failed';
          setError(reason);
          throw result.cause;
        }

        setHistoryEvents(result.events);
        setError(null);
      } finally {
        recovery?.signal.removeEventListener('abort', abort);
        if (historyAbort.current === controller) {
          historyAbort.current = null;
        }
      }
    },
    [client, enabled, namespace, workflowId]
  );

  // Initial back-fill + whenever the read target or workflow changes.
  useEffect(() => {
    void fetchHistory().catch(() => undefined);

    return () => {
      historyRequestGeneration.current += 1;
      historyAbort.current?.abort();
      historyAbort.current = null;
    };
  }, [fetchHistory]);

  // Live firehose subscription. On reconnect (onResync) we re-fetch describe and the
  // merged set re-collapses by seq, so the count cannot double-count.
  useEffect(() => {
    if (!enabled) {
      return;
    }

    setConnection(manager.getStatus());
    const stopStatus = manager.onStatusChange((status) => setConnection(status));

    const filter: FirehoseEventSubscriptionFilter = { kind: 'firehose', namespace };

    const onResync = async (context: ResyncContext) => {
      if (!context.isCurrent()) {
        return;
      }

      // Firehose has no resume cursor: drop the live buffer and back-fill fresh.
      liveSeqs.current = new Set();
      setLiveEvents([]);
      await fetchHistory(context);
    };

    const stopSubscription = manager.subscribe(
      filter,
      (event: Event) => {
        const seq = eventSequence(event);

        if (liveSeqs.current.has(seq)) {
          return;
        }

        liveSeqs.current.add(seq);
        setLiveEvents((previous) => [...previous, event]);
      },
      { onResync }
    );

    return () => {
      stopStatus();
      stopSubscription();
    };
  }, [enabled, manager, namespace, fetchHistory]);

  const events = useMemo(
    () => mergeEventsBySequence(historyEvents, liveEvents),
    [historyEvents, liveEvents]
  );

  const completedCount = useMemo(() => tallyExactlyOnce(events).count, [events]);
  const arity = useMemo(() => deriveArity(events, seedArity), [events, seedArity]);
  const status = useMemo(() => deriveStatus(events), [events]);
  const stale = connection !== 'connected';

  return {
    events,
    completedCount,
    arity,
    status,
    stale,
    error,
    refetch: () => void fetchHistory().catch(() => undefined),
  };
}

type HistoryFetchResult =
  | { kind: 'current'; events: Event[] }
  | { kind: 'stale' }
  | { kind: 'failed'; cause: unknown };

async function requestHistory(
  client: HistoryClient,
  workflowId: WorkflowId,
  namespace: Namespace,
  controller: AbortController,
  generation: number,
  currentGeneration: () => number,
  recovery?: ResyncContext
): Promise<HistoryFetchResult> {
  try {
    const events = await client.getHistory(workflowId, {
      namespace,
      signal: controller.signal,
    });
    if (isStaleHistoryRequest(generation, currentGeneration(), recovery)) {
      return { kind: 'stale' };
    }
    if (generation !== currentGeneration() || controller.signal.aborted) {
      return { kind: 'failed', cause: new Error('history request was superseded') };
    }
    return { kind: 'current', events };
  } catch (cause) {
    if (isStaleHistoryRequest(generation, currentGeneration(), recovery)) {
      return { kind: 'stale' };
    }
    return { kind: 'failed', cause };
  }
}

function isStaleHistoryRequest(
  generation: number,
  currentGeneration: number,
  recovery?: ResyncContext
): boolean {
  if (recovery !== undefined) {
    return !recovery.isCurrent();
  }
  return generation !== currentGeneration;
}
