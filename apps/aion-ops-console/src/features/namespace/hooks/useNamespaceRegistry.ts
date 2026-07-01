import { useEffect, useMemo, useState } from 'react';

import type { AionSocketError, ApiClient, ConnectionStatus, NamespaceRecord } from '@/lib/api';
import type { AionClusterStreamManager, ClusterStreamListener } from '@/lib/api/cluster-stream';
import { configuredClusterStream, createConfiguredApiClient } from '@/lib/config';
import type { ClusterEvent, ClusterSnapshot, NamespacePlacementWire } from '@/types';

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
 *
 * Control-Plane Phase 2 adds two more live folds on the SAME channel, no new
 * subscription and no polling:
 *
 * 3. A `NamespacePlacementChanged` delta (P2-I2) updates the matching row's
 *    `placement` in place, so an operator's `PUT .../placement` reflects live.
 * 4. A `NamespaceQuotaState` snapshot (P2-Q3) — pushed periodically by the server
 *    per active namespace — updates a per-namespace `{in_flight, ceiling}` overlay
 *    the panel renders as a live badge. The LATEST snapshot per namespace wins.
 */

/** Loading lifecycle of the initial `GET /namespaces/records` fetch. */
export type NamespaceLoadState = 'loading' | 'ready' | 'error';

/**
 * A namespace's live concurrency-quota state, folded from the server's periodic
 * `NamespaceQuotaState` push. `inFlight` is the durable Claimed outbox-row count
 * the keyed backpressure caps; `ceiling` is the tenant's cluster-wide contract.
 * A namespace with no snapshot yet has no entry (the badge renders "—" until the
 * first push arrives, typically within one cadence).
 */
export type NamespaceQuota = {
  inFlight: number;
  ceiling: number;
};

export type NamespaceRegistryResult = {
  /** The live namespace rows, ascending by `createdAt` then name. */
  namespaces: NamespaceRecord[];
  /**
   * Live per-namespace quota overlay, keyed by namespace name, folded from the
   * server's periodic `NamespaceQuotaState` push. Absent until the first snapshot.
   */
  quotas: Record<string, NamespaceQuota>;
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

/** A `NamespacePlacementChanged` delta narrowed off the cluster-event union. */
export type NamespacePlacementChangedEvent = Extract<
  ClusterEvent,
  { type: 'NamespacePlacementChanged' }
>;

export function isNamespacePlacementChanged(
  event: ClusterEvent
): event is NamespacePlacementChangedEvent {
  return event.type === 'NamespacePlacementChanged';
}

/** A `NamespaceQuotaState` snapshot narrowed off the cluster-event union. */
export type NamespaceQuotaStateEvent = Extract<ClusterEvent, { type: 'NamespaceQuotaState' }>;

export function isNamespaceQuotaState(event: ClusterEvent): event is NamespaceQuotaStateEvent {
  return event.type === 'NamespaceQuotaState';
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
    // A newly-minted namespace is Unplaced by default; a placement set later
    // arrives as a `NamespacePlacementChanged` delta and folds in place. The
    // `NamespaceCreated` delta carries no placement (it is a Phase-1 shape), so
    // the default here is the same durable default a fresh record holds.
    placement: { kind: 'unplaced', nodes: [] },
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
 * Fold a `NamespacePlacementChanged` delta into the rows: update the matching
 * namespace's `placement` in place (P2-I2). A delta for a namespace not yet in the
 * rows (a placement set before its `NamespaceCreated`/fetch landed) is dropped —
 * the record's own placement will arrive with the row and reflect the same durable
 * value, so nothing is lost. Returns the SAME array reference when the placement is
 * already equal by value, so React skips the re-render.
 */
export function applyPlacementDelta(
  rows: NamespaceRecord[],
  event: NamespacePlacementChangedEvent
): NamespaceRecord[] {
  const index = rows.findIndex((row) => row.name === event.name);
  if (index === -1) {
    return rows;
  }
  const current = rows[index];
  if (current === undefined || placementEquals(current.placement, event.placement)) {
    return rows;
  }
  const next = [...rows];
  next[index] = { ...current, placement: event.placement };
  return next;
}

/** Structural equality of two placement projections (kind + ordered node set). */
export function placementEquals(
  left: NamespacePlacementWire,
  right: NamespacePlacementWire
): boolean {
  return (
    left.kind === right.kind &&
    left.nodes.length === right.nodes.length &&
    left.nodes.every((node, index) => node === right.nodes[index])
  );
}

/**
 * Fold a `NamespaceQuotaState` snapshot into the quota overlay: the LATEST snapshot
 * per namespace wins (P2-Q3), superseding any prior value. Returns the SAME map
 * reference when the snapshot is unchanged by value so React skips the re-render.
 */
export function applyQuotaDelta(
  quotas: Record<string, NamespaceQuota>,
  event: NamespaceQuotaStateEvent
): Record<string, NamespaceQuota> {
  const current = quotas[event.namespace];
  if (
    current !== undefined &&
    current.inFlight === event.in_flight &&
    current.ceiling === event.ceiling
  ) {
    return quotas;
  }
  return {
    ...quotas,
    [event.namespace]: { inFlight: event.in_flight, ceiling: event.ceiling },
  };
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
  const [quotas, setQuotas] = useState<Record<string, NamespaceQuota>>({});
  const [loadState, setLoadState] = useState<NamespaceLoadState>('loading');
  const [loadError, setLoadError] = useState<Error | null>(null);
  const [status, setStatus] = useState<ConnectionStatus>(() => manager.getStatus());
  const [socketError, setSocketError] = useState<AionSocketError | null>(null);

  useEffect(() => {
    let active = true;
    const apiClient = options.apiClient ?? createConfiguredApiClient();

    const ingest = (event: ClusterEvent): void => {
      if (isNamespaceCreated(event)) {
        setNamespaces((rows) => applyDelta(rows, event));
        return;
      }
      if (isNamespacePlacementChanged(event)) {
        setNamespaces((rows) => applyPlacementDelta(rows, event));
        return;
      }
      if (isNamespaceQuotaState(event)) {
        setQuotas((current) => applyQuotaDelta(current, event));
      }
    };

    const listener: ClusterStreamListener = {
      // The cluster snapshot carries topology (peers/shards/workers), not the
      // namespace registry, so the panel's baseline is the REST fetch, not the
      // snapshot; we only fold the live namespace deltas here.
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
    () => ({ namespaces, quotas, loadState, loadError, status, socketError }),
    [namespaces, quotas, loadState, loadError, status, socketError]
  );
}
