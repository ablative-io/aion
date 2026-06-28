import { useQueries } from '@tanstack/react-query';

import type { ClusterNode } from '../lib/clusterConfig';

/**
 * Per-node `/metrics` poll. Scrapes the Prometheus `aion_connected_workers` gauge.
 *
 * Degradation contract (DEMO-VIEW-PLAN §1.1): a node whose metrics are missing or
 * unparseable has its worker row HIDDEN (`workers: null`), never zeroed — a hidden
 * row is honest about "we don't know", a `0` would lie that no workers are connected.
 * The error is surfaced on the row (`error`), never swallowed to a console warning.
 */

const METRICS_PATH = '/metrics';
const WORKER_GAUGE = 'aion_connected_workers';
const POLL_INTERVAL_MS = 2000;
const REQUEST_TIMEOUT_MS = 1500;

export type NodeMetrics = {
  nodeIndex: number;
  /** Connected-worker count, or null when the gauge is missing/unparseable. */
  workers: number | null;
  /** Human-readable error when the scrape failed; null on success. */
  error: string | null;
};

type FetchFn = typeof fetch;

export type UseNodeMetricsOptions = {
  nodes: readonly ClusterNode[];
  fetchImpl?: FetchFn;
  /** Set false to pause polling (e.g. when the tab is hidden). */
  enabled?: boolean;
};

/** Parse a Prometheus text-exposition body for the `aion_connected_workers` gauge. */
export function parseConnectedWorkers(body: string): number | null {
  for (const rawLine of body.split('\n')) {
    const line = rawLine.trim();

    if (line.length === 0 || line.startsWith('#') || !line.startsWith(WORKER_GAUGE)) {
      continue;
    }

    // Forms: `aion_connected_workers 3` or `aion_connected_workers{label="x"} 3`.
    const value = line.slice(line.lastIndexOf(' ') + 1);
    const parsed = Number.parseFloat(value);

    if (Number.isFinite(parsed)) {
      return parsed;
    }
  }

  return null;
}

async function fetchNodeMetrics(node: ClusterNode, fetchImpl: FetchFn): Promise<NodeMetrics> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);

  try {
    const response = await fetchImpl(`${node.baseUrl}${METRICS_PATH}`, {
      signal: controller.signal,
    });

    if (!response.ok) {
      return { nodeIndex: node.index, workers: null, error: `metrics HTTP ${response.status}` };
    }

    const workers = parseConnectedWorkers(await response.text());

    if (workers === null) {
      return { nodeIndex: node.index, workers: null, error: `${WORKER_GAUGE} gauge not present` };
    }

    return { nodeIndex: node.index, workers, error: null };
  } catch (cause) {
    const reason = cause instanceof Error ? cause.message : 'metrics request failed';
    return { nodeIndex: node.index, workers: null, error: reason };
  } finally {
    clearTimeout(timeout);
  }
}

export function nodeMetricsQueryKey(nodeIndex: number, baseUrl: string) {
  return ['failover', 'node-metrics', nodeIndex, baseUrl] as const;
}

/**
 * Poll every node's `/metrics` independently. Returns one `NodeMetrics` per node,
 * indexed parallel to `nodes`. A failing node never throws — it resolves to a row
 * with `workers: null` and a populated `error` so the UI can hide the row visibly.
 */
export function useNodeMetrics({
  nodes,
  fetchImpl = fetch,
  enabled = true,
}: UseNodeMetricsOptions): NodeMetrics[] {
  const results = useQueries({
    queries: nodes.map((node) => ({
      queryKey: nodeMetricsQueryKey(node.index, node.baseUrl),
      queryFn: () => fetchNodeMetrics(node, fetchImpl),
      refetchInterval: POLL_INTERVAL_MS,
      enabled,
      // fetchNodeMetrics never rejects, so no retry storm; one shot per interval.
      retry: false,
    })),
  });

  return results.map((result, index) => {
    const node = nodes[index];

    return (
      result.data ?? {
        nodeIndex: node?.index ?? index,
        workers: null,
        error: result.error instanceof Error ? result.error.message : null,
      }
    );
  });
}
