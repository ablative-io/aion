import { describe, expect, test } from 'bun:test';

import type { NamespaceRecord } from '@/lib/api';
import type { ClusterEvent } from '@/types';

import {
  applyDelta,
  isNamespaceCreated,
  mergeFetched,
  type NamespaceCreatedEvent,
  recordFromDelta,
  sortRecords,
} from './useNamespaceRegistry';

function created(name: string, createdAt: string, origin = 'worker_mint'): NamespaceCreatedEvent {
  return {
    type: 'NamespaceCreated',
    meta: { cluster_seq: 1, observed_at: createdAt },
    name,
    created_at: createdAt,
    origin,
  };
}

function row(name: string, createdAt: string, origin = 'explicit'): NamespaceRecord {
  return { name, createdAt, lastSeen: createdAt, origin };
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
