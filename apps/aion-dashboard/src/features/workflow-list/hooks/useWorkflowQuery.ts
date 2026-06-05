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

  return useQuery({
    queryKey: workflowQueryKey(namespace, filter, page),
    queryFn: () => client.queryWorkflows(filter, page, { namespace }),
    enabled: options.enabled ?? namespace.length > 0,
  });
}
