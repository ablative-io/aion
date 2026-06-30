import type { NodeLiveness } from '../hooks/useNodeLiveness';
import type { NodeMetrics } from '../hooks/useNodeMetrics';
import type { ClusterNode } from '../lib/clusterConfig';
import { NodeCard } from './NodeCard';

export type NodeGridProps = {
  nodes: readonly ClusterNode[];
  /** Parallel to `nodes`, keyed by index for lookup. */
  liveness: readonly NodeLiveness[];
  metrics: readonly NodeMetrics[];
  /** Index of the node that currently owns the demo shard (moves on adoption). */
  activeOwnerIndex: number | null;
  /** True before the kill has been observed (tags the seeded owner). */
  preKill: boolean;
};

function livenessFor(liveness: readonly NodeLiveness[], index: number): NodeLiveness {
  return (
    liveness.find((entry) => entry.nodeIndex === index) ?? {
      nodeIndex: index,
      state: 'unknown',
      error: null,
    }
  );
}

function metricsFor(metrics: readonly NodeMetrics[], index: number): NodeMetrics {
  return (
    metrics.find((entry) => entry.nodeIndex === index) ?? {
      nodeIndex: index,
      workers: null,
      error: null,
    }
  );
}

/** Lays out the N node cards in a responsive row. */
export function NodeGrid({ nodes, liveness, metrics, activeOwnerIndex, preKill }: NodeGridProps) {
  return (
    <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
      {nodes.map((node) => {
        const live = livenessFor(liveness, node.index);
        const metric = metricsFor(metrics, node.index);

        return (
          <NodeCard
            key={node.index}
            livenessError={live.error}
            metricsError={metric.error}
            node={node}
            ownsActiveShard={activeOwnerIndex === node.index}
            preKill={preKill}
            state={live.state}
            workers={metric.workers}
          />
        );
      })}
    </div>
  );
}
