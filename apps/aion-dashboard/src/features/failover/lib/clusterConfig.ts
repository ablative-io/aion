import { getDashboardConfig } from '@/lib/config';
import type { Namespace, WorkflowId } from '@/types';

/**
 * Static seed of the demo cluster topology. The demo (`scripts/demo/demo-failover.sh`)
 * is deterministic: node `i` owns shard `i`, listens on HTTP `8090 + i` and gRPC
 * `50051 + i`. The dashboard talks HTTP + WS only. No `/cluster/shards` endpoint
 * exists yet, so ownership is seeded here (documented gap, see DEMO-VIEW-PLAN §2.1).
 */

export const HTTP_BASE_PORT = 8090;
export const GRPC_BASE_PORT = 50051;
export const DEFAULT_NODE_COUNT = 3;

/**
 * Fallback namespace when none is supplied by the caller (selected namespace or
 * `?namespace=`). Reads the first configured namespace from the env-driven config
 * (the demo cluster uses `default`); only falls back to the literal `default`
 * when nothing is configured. No hardcoded `demo`.
 */
export function defaultClusterNamespace(): Namespace {
  const [first] = getDashboardConfig().namespaces;
  return first ?? ('default' as Namespace);
}

export type ClusterNode = {
  /** Stable zero-based index; also the seeded shard this node owns pre-kill. */
  index: number;
  label: string;
  httpPort: number;
  grpcPort: number;
  /** Base URL the dashboard uses for REST + WS against this node. */
  baseUrl: string;
  /** Shard this node owns in the seeded (pre-kill) topology. */
  ownedShard: number;
};

export type ClusterConfig = {
  nodes: readonly ClusterNode[];
  namespace: Namespace;
  /** Workflow id seeded from `?workflow=`; null means "discover via getWorkflowsPlain". */
  workflowId: WorkflowId | null;
  /** Index of the node that owns shard 0 — the kill target in the demo. */
  ownerIndex: number;
};

function nodeAt(index: number, host: string): ClusterNode {
  const httpPort = HTTP_BASE_PORT + index;

  return {
    index,
    label: `node ${index}`,
    httpPort,
    grpcPort: GRPC_BASE_PORT + index,
    baseUrl: `http://${host}:${httpPort}`,
    ownedShard: index,
  };
}

/** Parse `?nodes=` (a positive integer count); falls back to the demo default. */
export function parseNodeCount(raw: string | null): number {
  if (raw === null) {
    return DEFAULT_NODE_COUNT;
  }

  const parsed = Number.parseInt(raw, 10);

  if (!Number.isInteger(parsed) || parsed < 1) {
    return DEFAULT_NODE_COUNT;
  }

  return parsed;
}

/** Parse `?workflow=`; an empty/blank value means "not seeded" (discover later). */
export function parseWorkflowId(raw: string | null): WorkflowId | null {
  if (raw === null) {
    return null;
  }

  const trimmed = raw.trim();
  return trimmed.length === 0 ? null : trimmed;
}

export type ClusterConfigInput = {
  search?: string;
  host?: string;
  namespace?: Namespace;
  /** Fallback when `namespace` is absent; defaults to {@link defaultClusterNamespace}. */
  fallbackNamespace?: Namespace;
};

/**
 * Build the cluster config from URL query params (`?nodes=`, `?workflow=`).
 * Pure: callers pass `window.location.search` (or a stub in tests).
 */
export function buildClusterConfig(input: ClusterConfigInput = {}): ClusterConfig {
  const params = new URLSearchParams(input.search ?? '');
  const host = input.host ?? '127.0.0.1';
  const nodeCount = parseNodeCount(params.get('nodes'));
  const nodes = Array.from({ length: nodeCount }, (_, index) => nodeAt(index, host));

  return {
    nodes,
    namespace: input.namespace ?? input.fallbackNamespace ?? defaultClusterNamespace(),
    workflowId: parseWorkflowId(params.get('workflow')),
    ownerIndex: 0,
  };
}
