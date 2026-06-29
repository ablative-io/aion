import { useEffect, useState } from 'react';

import type { ApiClient } from '@/lib/api';
import { createConfiguredApiClient } from '@/lib/config';
import type { Namespace, WorkflowId } from '@/types';

/**
 * Discover a workflow to watch when the failover view was opened without an
 * explicit `?workflow=` seed (DEMO-VIEW-PLAN §2.1: "null means discover via
 * getWorkflowsPlain"). It picks the most recently started workflow in the
 * namespace, so the exactly-once counter back-fills from `POST
 * /workflows/describe` and shows the real completed count even with no live WS
 * events.
 *
 * A `seeded` id is returned unchanged (the operator's explicit choice wins). A
 * discovery failure is surfaced on `error` — never a silent empty state.
 */

type DiscoveryClient = Pick<ApiClient, 'getWorkflowsPlain'>;

export type UseDiscoveredWorkflowIdOptions = {
  namespace: Namespace;
  /** Explicit `?workflow=` seed; when non-null it is returned as-is. */
  seeded: WorkflowId | null;
  baseUrl?: string | undefined;
  apiClient?: DiscoveryClient | undefined;
  enabled?: boolean | undefined;
};

export type DiscoveredWorkflowId = {
  workflowId: WorkflowId | null;
  error: string | null;
};

function mostRecent(
  summaries: readonly { workflow_id: WorkflowId; started_at: string }[]
): WorkflowId | null {
  let latest: { workflow_id: WorkflowId; started_at: string } | null = null;
  for (const summary of summaries) {
    if (latest === null || summary.started_at > latest.started_at) {
      latest = summary;
    }
  }
  return latest?.workflow_id ?? null;
}

export function useDiscoveredWorkflowId({
  namespace,
  seeded,
  baseUrl,
  apiClient,
  enabled = true,
}: UseDiscoveredWorkflowIdOptions): DiscoveredWorkflowId {
  const [discovered, setDiscovered] = useState<WorkflowId | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!enabled || seeded !== null) {
      return;
    }

    const client =
      apiClient ??
      createConfiguredApiClient({ namespace, ...(baseUrl !== undefined && { baseUrl }) });

    let cancelled = false;
    client
      .getWorkflowsPlain({ namespace })
      .then((summaries) => {
        if (cancelled) {
          return;
        }
        setDiscovered(mostRecent(summaries));
        setError(null);
      })
      .catch((cause: unknown) => {
        if (cancelled) {
          return;
        }
        setError(cause instanceof Error ? cause.message : 'workflow discovery failed');
      });

    return () => {
      cancelled = true;
    };
  }, [apiClient, baseUrl, enabled, namespace, seeded]);

  return {
    workflowId: seeded ?? discovered,
    error,
  };
}
