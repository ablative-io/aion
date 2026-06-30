import { useMemo } from 'react';

import type { ClusterNode } from '../lib/clusterConfig';
import type { ClusterTopology } from './useClusterStream';

/**
 * Per-node liveness derived from the LIVE cluster stream — NOT a `/health/live`
 * poll. (WS3 deleted the 1 s health scrape: polling-dressed-as-realtime is a
 * real-data-rule violation.) Liveness now flows from the supervisor's `Peer*`
 * deltas reduced in {@link useClusterStream}: a node is `live` if it is the
 * reading (self) node or a connected peer, `dark` if a disconnected peer, and
 * `unknown` if the stream has not yet observed it (hollow dot — never silently
 * `live`).
 *
 * Own-read failover (§2.2): the view points its REST/WS reads at a target node
 * (default the configured owner). When that target goes `dark`/`unknown`, this
 * hook switches `activeTarget`/`activeBaseUrl` to the first `live` survivor
 * (lowest index) so killing node 0 does not blind the dashboard.
 */

export type NodeLivenessState = 'live' | 'dark' | 'unknown';

export type NodeLiveness = {
  nodeIndex: number;
  state: NodeLivenessState;
  /** Populated when the node is dark (the visible reason); null otherwise. */
  error: string | null;
};

export type UseNodeLivenessOptions = {
  nodes: readonly ClusterNode[];
  /** Index of the preferred read target (default owner). */
  preferredTargetIndex: number;
  /** Live topology from {@link useClusterStream}. */
  topology: ClusterTopology;
  /** True once a priming snapshot has been applied (pre-prime = all unknown). */
  primed: boolean;
};

export type UseNodeLivenessResult = {
  /** One entry per node, parallel to `nodes`. */
  liveness: NodeLiveness[];
  /** The node the view should read from now (own-read failover applied). */
  activeTarget: ClusterNode | null;
  /** Base URL of `activeTarget`, for REST + WS. Null when no node is live. */
  activeBaseUrl: string | null;
  /** True when reads have failed over off the preferred target to a survivor. */
  failedOver: boolean;
};

/**
 * Resolve a node's liveness from the reduced topology. The cluster stream keys
 * peers by node NAME; the demo's node labels (`node 0`...) are the names the
 * supervisor reports. A node matches the self node, a peer, by label.
 */
function livenessForNode(
  node: ClusterNode,
  topology: ClusterTopology,
  primed: boolean
): NodeLiveness {
  if (!primed) {
    return { nodeIndex: node.index, state: 'unknown', error: null };
  }

  if (topology.selfNode !== null && topology.selfNode === node.label) {
    return { nodeIndex: node.index, state: 'live', error: null };
  }

  const peer = topology.peers.find((entry) => entry.peer_name === node.label);

  if (peer === undefined) {
    return { nodeIndex: node.index, state: 'unknown', error: null };
  }

  if (peer.connected) {
    return { nodeIndex: node.index, state: 'live', error: null };
  }

  return {
    nodeIndex: node.index,
    state: 'dark',
    error: `peer down for ${peer.consecutive_down} tick${peer.consecutive_down === 1 ? '' : 's'}`,
  };
}

function selectActiveTarget(
  nodes: readonly ClusterNode[],
  liveness: readonly NodeLiveness[],
  preferredTargetIndex: number
): { target: ClusterNode | null; failedOver: boolean } {
  const preferred = nodes.find((node) => node.index === preferredTargetIndex);
  const preferredState = liveness.find((entry) => entry.nodeIndex === preferredTargetIndex)?.state;

  if (preferred !== undefined && preferredState === 'live') {
    return { target: preferred, failedOver: false };
  }

  const survivor = nodes.find(
    (node) => liveness.find((entry) => entry.nodeIndex === node.index)?.state === 'live'
  );

  if (survivor !== undefined) {
    return { target: survivor, failedOver: survivor.index !== preferredTargetIndex };
  }

  // Nobody is confirmed live. Hold the preferred target (do not invent a survivor).
  return { target: preferred ?? null, failedOver: false };
}

export function useNodeLiveness({
  nodes,
  preferredTargetIndex,
  topology,
  primed,
}: UseNodeLivenessOptions): UseNodeLivenessResult {
  const liveness = useMemo<NodeLiveness[]>(
    () => nodes.map((node) => livenessForNode(node, topology, primed)),
    [nodes, topology, primed]
  );

  const { target, failedOver } = useMemo(
    () => selectActiveTarget(nodes, liveness, preferredTargetIndex),
    [nodes, liveness, preferredTargetIndex]
  );

  return {
    liveness,
    activeTarget: target,
    activeBaseUrl: target?.baseUrl ?? null,
    failedOver,
  };
}
