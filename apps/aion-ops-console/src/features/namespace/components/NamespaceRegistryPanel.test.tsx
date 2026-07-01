import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import type { NamespaceRecord } from '@/lib/api';
import type { Namespace, NamespacePlacementWire } from '@/types';

import {
  applyPlacementDelta,
  applyQuotaDelta,
  type NamespacePlacementChangedEvent,
  type NamespaceQuota,
  type NamespaceQuotaStateEvent,
  type NamespaceRegistryResult,
} from '../hooks/useNamespaceRegistry';
import { NamespaceRegistryPanel } from './NamespaceRegistryPanel';

const UNPLACED: NamespacePlacementWire = { kind: 'unplaced', nodes: [] };

function record(name: string, placement: NamespacePlacementWire = UNPLACED): NamespaceRecord {
  return {
    name: name as Namespace,
    createdAt: '2026-06-30T00:00:00Z',
    lastSeen: '2026-06-30T00:00:00Z',
    origin: 'explicit',
    placement,
  };
}

function result(
  namespaces: NamespaceRecord[],
  quotas: Record<string, NamespaceQuota>
): NamespaceRegistryResult {
  return {
    namespaces,
    quotas,
    loadState: 'ready',
    loadError: null,
    status: 'connected',
    socketError: null,
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

const noopSetPlacement = async () => {};

describe('NamespaceRegistryPanel — Phase 2 placement + quota', () => {
  test('renders each namespace placement honestly (pinned = hard/isolated)', () => {
    const rows = [record('orders', { kind: 'pinned', nodes: ['node-a', 'node-b'] })];
    const markup = renderToStaticMarkup(
      <NamespaceRegistryPanel registry={result(rows, {})} onSetPlacement={noopSetPlacement} />
    );

    expect(markup).toContain('Pinned (hard)');
    // The node label set is shown so the operator sees the isolation target.
    expect(markup).toContain('node-a, node-b');
    // The isolation meaning is surfaced (Pinned = require these nodes / isolated).
    expect(markup).toContain('isolated');
  });

  test('folds a NamespacePlacementChanged delta, then renders the new placement live', () => {
    // Start Unplaced, apply the REAL fold reducer, render the folded rows: the
    // panel reflects the live delta with no refetch (the socket-first contract).
    const rows = [record('orders', UNPLACED)];
    const folded = applyPlacementDelta(
      rows,
      placementChanged('orders', { kind: 'prefer', nodes: ['zone-1'] })
    );

    const markup = renderToStaticMarkup(
      <NamespaceRegistryPanel registry={result(folded, {})} onSetPlacement={noopSetPlacement} />
    );

    expect(markup).toContain('Prefer (soft)');
    expect(markup).toContain('zone-1');
  });

  test('folds a quota-state delta into a live in-flight/ceiling badge', () => {
    const rows = [record('orders')];
    const quotas = applyQuotaDelta({}, quotaState('orders', 3, 8));

    const markup = renderToStaticMarkup(
      <NamespaceRegistryPanel registry={result(rows, quotas)} onSetPlacement={noopSetPlacement} />
    );

    expect(markup).toContain('3 / 8');
    // Below ceiling: not throttled.
    expect(markup).not.toContain('throttled');
  });

  test('an at-ceiling quota snapshot renders the throttled state', () => {
    const rows = [record('orders')];
    // Two snapshots fold; the latest (at ceiling) wins and the badge turns
    // throttled — the operator sees the ceiling bite live.
    const quotas = applyQuotaDelta(
      applyQuotaDelta({}, quotaState('orders', 5, 8)),
      quotaState('orders', 8, 8)
    );

    const markup = renderToStaticMarkup(
      <NamespaceRegistryPanel registry={result(rows, quotas)} onSetPlacement={noopSetPlacement} />
    );

    expect(markup).toContain('8 / 8');
    expect(markup).toContain('throttled');
  });

  test('a namespace with no quota snapshot yet shows an honest placeholder, not a fake zero', () => {
    const rows = [record('orders')];
    const markup = renderToStaticMarkup(
      <NamespaceRegistryPanel registry={result(rows, {})} onSetPlacement={noopSetPlacement} />
    );

    expect(markup).toContain('Quota pending first snapshot');
    expect(markup).not.toContain('0 / 0');
  });
});
