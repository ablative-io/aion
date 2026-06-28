import { useQueries } from '@tanstack/react-query';
import { useMemo } from 'react';

import type { ClusterNode } from '../lib/clusterConfig';

/**
 * Per-node `/health/live` poll → `live | dark | unknown`, plus own-read failover.
 *
 * Liveness contract (DEMO-VIEW-PLAN §1.1, §2.1): a node that answers 200 is `live`;
 * a node that times out or answers non-200 is `dark`; a node not yet polled is
 * `unknown` (hollow dot, "no health response") — NEVER silently `live`. The poll is
 * the degradation surface, so a failed probe resolves to `dark` rather than throwing.
 *
 * Own-read failover (§2.2): the view points its REST/WS reads at a target node
 * (default the configured owner). When that target goes `dark`, this hook switches
 * `activeTarget`/`activeBaseUrl` to the first `live` survivor (lowest index) so
 * killing node 0 does not blind the dashboard.
 */

const HEALTH_PATH = '/health/live';
const POLL_INTERVAL_MS = 1000;
const REQUEST_TIMEOUT_MS = 800;

export type NodeLivenessState = 'live' | 'dark' | 'unknown';

export type NodeLiveness = {
  nodeIndex: number;
  state: NodeLivenessState;
  /** Populated when the probe failed (the visible reason); null when live/unknown. */
  error: string | null;
};

type FetchFn = typeof fetch;

export type UseNodeLivenessOptions = {
  nodes: readonly ClusterNode[];
  /** Index of the preferred read target (default owner). */
  preferredTargetIndex: number;
  fetchImpl?: FetchFn;
  enabled?: boolean;
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

async function probeNode(node: ClusterNode, fetchImpl: FetchFn): Promise<NodeLiveness> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);

  try {
    const response = await fetchImpl(`${node.baseUrl}${HEALTH_PATH}`, {
      signal: controller.signal,
    });

    if (!response.ok) {
      return { nodeIndex: node.index, state: 'dark', error: `health HTTP ${response.status}` };
    }

    return { nodeIndex: node.index, state: 'live', error: null };
  } catch (cause) {
    const reason = cause instanceof Error ? cause.message : 'health probe failed';
    return { nodeIndex: node.index, state: 'dark', error: reason };
  } finally {
    clearTimeout(timeout);
  }
}

export function nodeLivenessQueryKey(nodeIndex: number, baseUrl: string) {
  return ['failover', 'node-liveness', nodeIndex, baseUrl] as const;
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

  // Preferred is dark/unknown — fail over to the lowest-index live survivor.
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
  fetchImpl = fetch,
  enabled = true,
}: UseNodeLivenessOptions): UseNodeLivenessResult {
  const results = useQueries({
    queries: nodes.map((node) => ({
      queryKey: nodeLivenessQueryKey(node.index, node.baseUrl),
      queryFn: () => probeNode(node, fetchImpl),
      refetchInterval: POLL_INTERVAL_MS,
      enabled,
      retry: false,
    })),
  });

  const liveness = useMemo<NodeLiveness[]>(
    () =>
      results.map((result, index) => {
        const node = nodes[index];

        if (result.data !== undefined) {
          return result.data;
        }

        if (result.isError) {
          const reason = result.error instanceof Error ? result.error.message : 'probe failed';
          return { nodeIndex: node?.index ?? index, state: 'dark', error: reason };
        }

        return { nodeIndex: node?.index ?? index, state: 'unknown', error: null };
      }),
    [results, nodes]
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
