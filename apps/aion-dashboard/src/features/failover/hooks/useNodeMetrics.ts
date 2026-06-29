import { useMemo } from 'react';

import type { ClusterNode } from '../lib/clusterConfig';
import type { ClusterTopology } from './useClusterStream';

/**
 * Per-node connected-worker counts derived from the LIVE cluster stream — NOT a
 * `/metrics` scrape. (WS3 deleted the 2 s Prometheus poll: it was
 * polling-dressed-as-realtime.) The count is reduced from the
 * `WorkerConnected`/`WorkerDisconnected` deltas in {@link useClusterStream}
 * (`topology.workersByNode`), keyed by the worker's reported `node` label.
 *
 * Degradation contract (§1.1): before the priming snapshot is applied a node's
 * worker count is HIDDEN (`workers: null`), never zeroed — a hidden row is honest
 * about "not yet observed", a `0` would lie that no workers are connected. Once
 * primed, a node with no workers truthfully shows `0`.
 */

export type NodeMetrics = {
  nodeIndex: number;
  /** Connected-worker count, or null when not yet observed (pre-prime). */
  workers: number | null;
  /** Human-readable note when the count is unavailable; null otherwise. */
  error: string | null;
};

export type UseNodeMetricsOptions = {
  nodes: readonly ClusterNode[];
  /** Live topology from {@link useClusterStream}. */
  topology: ClusterTopology;
  /** True once a priming snapshot has been applied. */
  primed: boolean;
};

/**
 * Project per-node worker counts from the reduced cluster topology. Workers
 * report a `node` label; counts are tallied per label. A node with no observed
 * workers reads `0` once primed (the stream has authoritatively said so).
 */
export function useNodeMetrics({ nodes, topology, primed }: UseNodeMetricsOptions): NodeMetrics[] {
  return useMemo<NodeMetrics[]>(
    () =>
      nodes.map((node) => {
        if (!primed) {
          return { nodeIndex: node.index, workers: null, error: 'awaiting cluster snapshot' };
        }

        return {
          nodeIndex: node.index,
          workers: topology.workersByNode.get(node.label) ?? 0,
          error: null,
        };
      }),
    [nodes, topology, primed]
  );
}
