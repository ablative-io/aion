import { useEffect, useMemo, useState } from 'react';

import type { ApiClient, AionSocketError, ConnectionStatus, NamespaceRecord } from '@/lib/api';
import type { AionClusterStreamManager, ClusterStreamListener } from '@/lib/api/cluster-stream';
import { configuredClusterStream, createConfiguredApiClient } from '@/lib/config';
import type { ClusterEvent, ClusterSnapshot } from '@/types';

/**
 * Live namespace registry hook (Control-Plane Phase 1, S8).
 *
 * The single source of the ops console's namespace panel. It is socket-first and
 * REAL-DATA-ONLY:
 *
 * 1. On mount it loads the REAL current durable set from `GET /namespaces/records`
 *    (created_at / last_seen / origin columns, grant-filtered + existence-leak
 *    safe server-side).
 * 2. It then subscribes to the SAME deploy-scoped cluster push channel the
 *    failover view uses and appends/updates rows LIVE from the real
 *    `NamespaceCreated` delta — no refresh, no polling, no `setInterval`.
 *
 * There is NO mock data and NO hardcoded namespace list anywhere: the initial
 * rows come from the endpoint and every subsequent row arrives as a server delta.
 * A `NamespaceCreated` for a name already present (a benign duplicate across a
 * reconnect re-prime, or a record we already fetched) updates in place rather
 * than duplicating, so each namespace appears exactly once.
 */

/** Loading lifecycle of the initial `GET /namespaces/records` fetch. */
export type NamespaceLoadState = 'loading' | 'ready' | 'error';

export type NamespaceRegistryResult = {
  /** The live namespace rows, ascending by `createdAt` then name. */
  namespaces: NamespaceRecord[];
  /** Lifecycle of the initial durable-set fetch. */
  loadState: NamespaceLoadState;
  /** The initial-fetch error, when `loadState === 'error'`. */
  loadError: Error | null;
  /** Live cluster-socket connection status (the delta channel). */
  status: ConnectionStatus;
  /** Typed live-socket error, or null (no-silent-failure). */
  socketError: AionSocketError | null;
};

export type UseNamespaceRegistryOptions = {
  /** Override the REST client (tests); defaults to the configured client. */
  apiClient?: Pick<ApiClient, 'listNamespaceRecords'>;
  /** Override the cluster-stream manager (tests); defaults to the singleton. */
  manager?: Pick<
    AionClusterStreamManager,
    'subscribe' | 'onStatusChange' | 'getStatus' | 'onError'
  >;
};

/** A `NamespaceCreated` delta narrowed off the cluster-event union. */
export type NamespaceCreatedEvent = Extract<ClusterEvent, { type: 'NamespaceCreated' }>;

export function isNamespaceCreated(event: ClusterEvent): event is NamespaceCreatedEvent {
  return event.type === 'NamespaceCreated';
}

/** Map a live `NamespaceCreated` delta to a panel row. */
export function recordFromDelta(event: NamespaceCreatedEvent): NamespaceRecord {
  return {
    name: event.name,
    createdAt: event.created_at,
    // The delta carries the creation instant; on first appearance last_seen
    // equals created_at (the namespace has been seen exactly once, at mint).
    lastSeen: event.created_at,
    origin: event.origin,
  };
}

/** Stable ascending sort by createdAt then name (the registry's list order). */
export function sortRecords(records: NamespaceRecord[]): NamespaceRecord[] {
  return [...records].sort(
    (left, right) =>
      left.createdAt.localeCompare(right.createdAt) || left.name.localeCompare(right.name)
  );
}

/**
 * Fold a delta into the current rows: upsert by name (existing row keeps its
 * fetched created_at/last_seen, never regressing to the delta's values), append
 * when new. Returns the same array reference when nothing changed so React can
 * skip a re-render.
 */
export function applyDelta(
  rows: NamespaceRecord[],
  event: NamespaceCreatedEvent
): NamespaceRecord[] {
  if (rows.some((row) => row.name === event.name)) {
    return rows;
  }

  return sortRecords([...rows, recordFromDelta(event)]);
}

/**
 * Merge the fetched durable rows UNDER any deltas already folded into state while
 * the fetch was in flight: a namespace minted during the fetch (already present
 * as a delta-derived row) is neither dropped nor duplicated — the fetched record
 * wins on name collision (it is the authoritative created_at/last_seen), and any
 * buffered-only row is preserved. Returns the merged, sorted set.
 */
export function mergeFetched(
  buffered: NamespaceRecord[],
  fetched: NamespaceRecord[]
): NamespaceRecord[] {
  const byName = new Map(fetched.map((record) => [record.name, record]));
  for (const row of buffered) {
    if (!byName.has(row.name)) {
      byName.set(row.name, row);
    }
  }
  return sortRecords([...byName.values()]);
}

export function useNamespaceRegistry(
  options: UseNamespaceRegistryOptions = {}
): NamespaceRegistryResult {
  const manager = options.manager ?? configuredClusterStream;
  const [namespaces, setNamespaces] = useState<NamespaceRecord[]>([]);
  const [loadState, setLoadState] = useState<NamespaceLoadState>('loading');
  const [loadError, setLoadError] = useState<Error | null>(null);
  const [status, setStatus] = useState<ConnectionStatus>(() => manager.getStatus());
  const [socketError, setSocketError] = useState<AionSocketError | null>(null);

  useEffect(() => {
    let active = true;
    const apiClient = options.apiClient ?? createConfiguredApiClient();

    const ingest = (event: ClusterEvent): void => {
      if (!isNamespaceCreated(event)) {
        return;
      }
      setNamespaces((rows) => applyDelta(rows, event));
    };

    const listener: ClusterStreamListener = {
      // The cluster snapshot carries topology (peers/shards/workers), not the
      // namespace registry, so the panel's baseline is the REST fetch, not the
      // snapshot; we only fold the live NamespaceCreated deltas here.
      onSnapshot: (_snapshot: ClusterSnapshot) => {},
      onEvent: ingest,
    };

    const unsubscribeStream = manager.subscribe(listener);
    const unsubscribeStatus = manager.onStatusChange((next) => {
      if (active) {
        setStatus(next);
      }
    });
    const unsubscribeError = manager.onError((next) => {
      if (active) {
        setSocketError(next);
      }
    });
    setStatus(manager.getStatus());

    apiClient
      .listNamespaceRecords()
      .then((records) => {
        if (!active) {
          return;
        }
        // Merge fetched rows UNDER any deltas already buffered into state, so a
        // namespace minted during the fetch is not dropped or duplicated.
        setNamespaces((buffered) => mergeFetched(buffered, records));
        setLoadState('ready');
        setLoadError(null);
      })
      .catch((error: unknown) => {
        if (!active) {
          return;
        }
        setLoadState('error');
        setLoadError(error instanceof Error ? error : new Error(String(error)));
      });

    return () => {
      active = false;
      unsubscribeStream();
      unsubscribeStatus();
      unsubscribeError();
    };
  }, [manager, options.apiClient]);

  return useMemo(
    () => ({ namespaces, loadState, loadError, status, socketError }),
    [namespaces, loadState, loadError, status, socketError]
  );
}
