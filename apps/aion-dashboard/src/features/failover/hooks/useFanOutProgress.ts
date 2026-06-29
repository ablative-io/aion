import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { eventSequence, mergeEventsBySequence } from '@/features/workflow-detail/lib/timeline';
import type { AionEventWebSocketManager, ApiClient } from '@/lib/api';
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

export type FanOutConnection = 'connected' | 'reconnecting' | 'disconnected';

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
  baseUrl?: string;
  apiClient?: HistoryClient;
  manager?: Manager;
  /** Seed arity when no ActivityScheduled events are present yet (e.g. collect_four → 4). */
  seedArity?: number | null;
  enabled?: boolean;
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

  const fetchHistory = useCallback(() => {
    if (!enabled || workflowId === null) {
      return;
    }

    client
      .getHistory(workflowId, { namespace })
      .then((events) => {
        setHistoryEvents(events);
        setError(null);
      })
      .catch((cause: unknown) => {
        const reason = cause instanceof Error ? cause.message : 'describe back-fill failed';
        setError(reason);
      });
  }, [client, enabled, namespace, workflowId]);

  // Initial back-fill + whenever the read target or workflow changes.
  useEffect(() => {
    fetchHistory();
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

    const onResync = (_context: ResyncContext) => {
      // Firehose has no resume cursor: drop the live buffer and back-fill fresh.
      liveSeqs.current = new Set();
      setLiveEvents([]);
      fetchHistory();
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
    refetch: fetchHistory,
  };
}
