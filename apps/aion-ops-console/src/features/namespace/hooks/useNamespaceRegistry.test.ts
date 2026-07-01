import { describe, expect, test } from 'bun:test';

import type { NamespaceRecord } from '@/lib/api';
import type { ClusterEvent, NamespacePlacementWire } from '@/types';

import {
  applyDelta,
  applyPlacementDelta,
  applyQuotaDelta,
  isNamespaceCreated,
  isNamespacePlacementChanged,
  isNamespaceQuotaState,
  mergeFetched,
  type NamespaceCreatedEvent,
  type NamespacePlacementChangedEvent,
  type NamespaceQuota,
  type NamespaceQuotaStateEvent,
  placementEquals,
  recordFromDelta,
  sortRecords,
} from './useNamespaceRegistry';

const UNPLACED: NamespacePlacementWire = { kind: 'unplaced', nodes: [] };

function created(name: string, createdAt: string, origin = 'worker_mint'): NamespaceCreatedEvent {
  return {
    type: 'NamespaceCreated',
    meta: { cluster_seq: 1, observed_at: createdAt },
    name,
    created_at: createdAt,
    origin,
  };
}

function placementChanged(
  name: string,
  placement: NamespacePlacementWire
): NamespacePlacementChangedEvent {
  return {
    type: 'NamespacePlacementChanged',
    meta: { cluster_seq: 2, observed_at: '2026-06-30T00:00:00Z' },
    name,
    placement,
  };
}

function quotaState(
  namespace: string,
  inFlight: number,
  ceiling: number
): NamespaceQuotaStateEvent {
  return {
    type: 'NamespaceQuotaState',
    meta: { cluster_seq: 3, observed_at: '2026-06-30T00:00:00Z' },
    namespace,
    in_flight: inFlight,
    ceiling,
  };
}

function row(
  name: string,
  createdAt: string,
  origin = 'explicit',
  placement: NamespacePlacementWire = UNPLACED
): NamespaceRecord {
  return { name, createdAt, lastSeen: createdAt, origin, placement };
}

describe('isNamespaceCreated', () => {
  test('narrows only the NamespaceCreated variant', () => {
    expect(isNamespaceCreated(created('orders', '2026-06-30T00:00:00Z'))).toBe(true);
    const supervisor: ClusterEvent = {
      type: 'SupervisorStarted',
      meta: { cluster_seq: 2, observed_at: '2026-06-30T00:00:00Z' },
      node: 'node-1',
    };
    expect(isNamespaceCreated(supervisor)).toBe(false);
  });
});

describe('recordFromDelta', () => {
  test('maps a delta to a row with last_seen equal to created_at on first appearance', () => {
    const delta = created('billing', '2026-06-30T01:02:03Z', 'start_mint');
    expect(recordFromDelta(delta)).toEqual({
      name: 'billing',
      createdAt: '2026-06-30T01:02:03Z',
      lastSeen: '2026-06-30T01:02:03Z',
      origin: 'start_mint',
      placement: { kind: 'unplaced', nodes: [] },
    });
  });
});

describe('applyDelta', () => {
  test('appends a genuinely-new namespace and keeps the list sorted by createdAt', () => {
    const rows = [row('alpha', '2026-06-30T00:00:00Z')];
    const next = applyDelta(rows, created('zeta', '2026-06-29T00:00:00Z'));

    expect(next.map((record) => record.name)).toEqual(['zeta', 'alpha']);
  });

  test('is idempotent: a delta for an already-present name returns the SAME array (no duplicate)', () => {
    const rows = sortRecords([row('orders', '2026-06-30T00:00:00Z')]);
    const next = applyDelta(rows, created('orders', '2026-06-30T09:09:09Z'));

    // Same reference (React skips the re-render) and no duplicate row.
    expect(next).toBe(rows);
    expect(next.filter((record) => record.name === 'orders')).toHaveLength(1);
  });
});

describe('mergeFetched', () => {
  test('keeps a delta-only row that arrived during the fetch, sorted, no duplicate', () => {
    // `live` arrived as a delta while the fetch was in flight; the fetch returns
    // the older `seed`. Both survive exactly once, sorted by createdAt.
    const buffered = [row('live', '2026-06-30T12:00:00Z', 'worker_mint')];
    const fetched = [row('seed', '2026-06-30T00:00:00Z')];

    const merged = mergeFetched(buffered, fetched);
    expect(merged.map((record) => record.name)).toEqual(['seed', 'live']);
  });

  test('fetched record wins on a name collision (authoritative created_at)', () => {
    const buffered = [row('orders', '2026-06-30T12:00:00Z', 'worker_mint')];
    const fetched = [row('orders', '2026-06-30T00:00:00Z', 'explicit')];

    const merged = mergeFetched(buffered, fetched);
    expect(merged).toHaveLength(1);
    expect(merged[0]?.createdAt).toBe('2026-06-30T00:00:00Z');
    expect(merged[0]?.origin).toBe('explicit');
  });
});

describe('sortRecords', () => {
  test('orders by createdAt then name, breaking ties deterministically', () => {
    const rows = [
      row('b', '2026-06-30T00:00:00Z'),
      row('a', '2026-06-30T00:00:00Z'),
      row('early', '2026-06-29T00:00:00Z'),
    ];
    expect(sortRecords(rows).map((record) => record.name)).toEqual(['early', 'a', 'b']);
  });
});

describe('cluster-event narrowing (Phase 2 folds)', () => {
  test('narrows placement-changed and quota-state variants distinctly', () => {
    const placement = placementChanged('orders', { kind: 'pinned', nodes: ['node-a'] });
    const quota = quotaState('orders', 3, 8);
    expect(isNamespacePlacementChanged(placement)).toBe(true);
    expect(isNamespaceQuotaState(placement)).toBe(false);
    expect(isNamespaceQuotaState(quota)).toBe(true);
    expect(isNamespacePlacementChanged(quota)).toBe(false);
    expect(isNamespaceCreated(placement)).toBe(false);
  });
});

describe('applyPlacementDelta (P2-I2)', () => {
  test('folds a placement change into the matching row in place, live', () => {
    const rows = [row('orders', '2026-06-30T00:00:00Z')];
    const pinned: NamespacePlacementWire = { kind: 'pinned', nodes: ['node-a', 'node-b'] };
    const next = applyPlacementDelta(rows, placementChanged('orders', pinned));

    expect(next).not.toBe(rows);
    expect(next[0]?.placement).toEqual(pinned);
    // Identity fields are untouched — only placement moved.
    expect(next[0]?.name).toBe('orders');
    expect(next[0]?.createdAt).toBe('2026-06-30T00:00:00Z');
  });

  test('is a no-op (same reference) when the placement is already equal by value', () => {
    const prefer: NamespacePlacementWire = { kind: 'prefer', nodes: ['node-a'] };
    const rows = [row('orders', '2026-06-30T00:00:00Z', 'explicit', prefer)];
    const next = applyPlacementDelta(rows, placementChanged('orders', { ...prefer }));

    expect(next).toBe(rows);
  });

  test('drops a delta for a namespace not yet in the rows (no phantom row)', () => {
    const rows = [row('orders', '2026-06-30T00:00:00Z')];
    const next = applyPlacementDelta(rows, placementChanged('unknown', UNPLACED));

    expect(next).toBe(rows);
    expect(next).toHaveLength(1);
  });
});

describe('applyQuotaDelta (P2-Q3)', () => {
  test('folds the latest snapshot per namespace, superseding the prior value', () => {
    const first = applyQuotaDelta({}, quotaState('orders', 2, 8));
    expect(first.orders).toEqual({ inFlight: 2, ceiling: 8 });

    const second = applyQuotaDelta(first, quotaState('orders', 7, 8));
    expect(second.orders).toEqual({ inFlight: 7, ceiling: 8 });
    expect(second).not.toBe(first);
  });

  test('at-ceiling snapshot is folded verbatim (the badge derives throttled state)', () => {
    const quotas: Record<string, NamespaceQuota> = {};
    const next = applyQuotaDelta(quotas, quotaState('orders', 8, 8));
    expect(next.orders).toEqual({ inFlight: 8, ceiling: 8 });
  });

  test('is a no-op (same reference) when the snapshot is unchanged by value', () => {
    const quotas = applyQuotaDelta({}, quotaState('orders', 3, 8));
    const next = applyQuotaDelta(quotas, quotaState('orders', 3, 8));
    expect(next).toBe(quotas);
  });

  test('keeps independent snapshots for distinct namespaces', () => {
    const quotas = applyQuotaDelta(
      applyQuotaDelta({}, quotaState('orders', 1, 8)),
      quotaState('billing', 5, 5)
    );
    expect(quotas.orders).toEqual({ inFlight: 1, ceiling: 8 });
    expect(quotas.billing).toEqual({ inFlight: 5, ceiling: 5 });
  });
});

describe('placementEquals', () => {
  test('compares kind and ordered node set', () => {
    expect(
      placementEquals({ kind: 'prefer', nodes: ['a'] }, { kind: 'prefer', nodes: ['a'] })
    ).toBe(true);
    expect(
      placementEquals({ kind: 'prefer', nodes: ['a'] }, { kind: 'pinned', nodes: ['a'] })
    ).toBe(false);
    expect(
      placementEquals({ kind: 'prefer', nodes: ['a', 'b'] }, { kind: 'prefer', nodes: ['b', 'a'] })
    ).toBe(false);
  });
});
