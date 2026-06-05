import { useQuery, type UseQueryResult } from '@tanstack/react-query';

import {
  createApiClient,
  type ApiClient,
  type WorkflowPage,
  type WorkflowPageRequest,
} from '@/lib/api';
import type { Namespace, WorkflowFilter, WorkflowSummary } from '@/types';

const defaultClient = createApiClient();

export type UseWorkflowQueryOptions = {
  client?: ApiClient;
  enabled?: boolean;
};

export type WorkflowQueryKey = readonly [
  'workflows',
  Namespace,
  WorkflowFilter,
  WorkflowPageRequest,
];

export function workflowQueryKey(
  namespace: Namespace,
  filter: WorkflowFilter,
  page: WorkflowPageRequest
): WorkflowQueryKey {
  return ['workflows', namespace, filter, page] as const;
}

export function useWorkflowQuery(
  namespace: Namespace,
  filter: WorkflowFilter,
  page: WorkflowPageRequest,
  options: UseWorkflowQueryOptions = {}
): UseQueryResult<WorkflowPage<WorkflowSummary>, Error> {
  const client = options.client ?? defaultClient;
  const normalizedPage = normalizeWorkflowPageRequest(page);

  return useQuery({
    queryKey: workflowQueryKey(namespace, filter, normalizedPage),
    queryFn: () => queryWorkflowPage(client, namespace, filter, normalizedPage),
    enabled: options.enabled ?? namespace.length > 0,
  });
}

export function queryWorkflowPage(
  client: ApiClient,
  namespace: Namespace,
  filter: WorkflowFilter,
  page: WorkflowPageRequest
): Promise<WorkflowPage<WorkflowSummary>> {
  return client.queryWorkflows(filter, page, { namespace });
}

function normalizeWorkflowPageRequest(page: WorkflowPageRequest): WorkflowPageRequest {
  return {
    cursor: page.cursor,
    limit: normalizePageLimit(page.limit),
  };
}

function normalizePageLimit(limit: number | undefined): number | undefined {
  if (limit === undefined) {
    return undefined;
  }

  if (!Number.isFinite(limit)) {
    return undefined;
  }

  return Math.max(1, Math.floor(limit));
}
