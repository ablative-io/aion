import { useMemo } from 'react';

import { useWorkflowQuery } from '@/features/workflow-list';
import type { ApiClient } from '@/lib/api';
import type { WorkflowFilter, WorkflowSummary } from '@/types';

import { GATED_FEEDS, type GatedFeed } from '../lib/gatedFeeds';
import {
  deriveWorkflowIncidents,
  type Incident,
  STUCK_RUNNING_THRESHOLD_MS,
} from '../lib/incidents';

const TRIAGE_PAGE_LIMIT = 50;

const FAILED_FILTER: WorkflowFilter = {
  workflow_type: null,
  status: 'Failed',
  started_after: null,
  started_before: null,
  parent: null,
};

const TIMED_OUT_FILTER: WorkflowFilter = { ...FAILED_FILTER, status: 'TimedOut' };

const RUNNING_FILTER: WorkflowFilter = { ...FAILED_FILTER, status: 'Running' };

export type UseIncidentsOptions = {
  /** Override for tests; defaults to the slice's own default ApiClient. */
  apiClient?: ApiClient;
  /** Injectable clock for deterministic stuck-detection tests. */
  now?: number;
  /** Override the stuck-running age threshold (ms). */
  stuckThresholdMs?: number;
};

export type IncidentsState = {
  incidents: Incident[];
  /** True while any underlying list query is still loading its first page. */
  isLoading: boolean;
  /** True if any underlying list query errored — surfaced to visible state, never swallowed. */
  isError: boolean;
  /** The first error encountered, for the visible error affordance. */
  error: unknown;
  /** Re-run every underlying query. */
  refetch: () => void;
  /** Incident classes the server does not feed yet, surfaced honestly (no fake data). */
  gatedFeeds: readonly GatedFeed[];
};

/**
 * Triage incident source (plan §4.4 / slice S7). Ranks failed/timed-out and
 * stuck-running workflows from the workflow LIST query. Worker / outbox /
 * node-shard / fenced incident classes are gated behind feed availability: the
 * required subscription kinds are not on the wire, so this hook emits none of
 * them and instead reports {@link GATED_FEEDS} for an honest "awaiting server
 * support" surface. No incident is ever fabricated.
 */
export function useIncidents(options: UseIncidentsOptions = {}): IncidentsState {
  const { apiClient, stuckThresholdMs = STUCK_RUNNING_THRESHOLD_MS } = options;

  const failed = useWorkflowQuery({
    apiClient,
    filter: FAILED_FILTER,
    page: { limit: TRIAGE_PAGE_LIMIT },
  });
  const timedOut = useWorkflowQuery({
    apiClient,
    filter: TIMED_OUT_FILTER,
    page: { limit: TRIAGE_PAGE_LIMIT },
  });
  const running = useWorkflowQuery({
    apiClient,
    filter: RUNNING_FILTER,
    page: { limit: TRIAGE_PAGE_LIMIT },
  });

  const queries = [failed, timedOut, running] as const;

  const incidents = useMemo(() => {
    const summaries: WorkflowSummary[] = [
      ...(failed.data?.items ?? []),
      ...(timedOut.data?.items ?? []),
      ...(running.data?.items ?? []),
    ];
    const now = options.now ?? Date.now();
    return dedupeById(deriveWorkflowIncidents(summaries, now, stuckThresholdMs));
  }, [failed.data, timedOut.data, running.data, options.now, stuckThresholdMs]);

  const firstError = queries.find((query) => query.isError)?.error ?? null;

  return {
    incidents,
    isLoading: queries.some((query) => query.isPending),
    isError: queries.some((query) => query.isError),
    error: firstError,
    refetch: () => {
      for (const query of queries) {
        void query.refetch();
      }
    },
    gatedFeeds: GATED_FEEDS,
  };
}

/**
 * Defensive dedup by incident id. A workflow appears in exactly one status
 * bucket on the wire, so collisions should not occur, but if the server returns
 * an overlapping page we keep the first (highest-ranked, since the input is
 * already rank-sorted) rather than render a duplicate card.
 */
function dedupeById(incidents: readonly Incident[]): Incident[] {
  const seen = new Set<string>();
  const result: Incident[] = [];
  for (const incident of incidents) {
    if (seen.has(incident.id)) {
      continue;
    }
    seen.add(incident.id);
    result.push(incident);
  }
  return result;
}
