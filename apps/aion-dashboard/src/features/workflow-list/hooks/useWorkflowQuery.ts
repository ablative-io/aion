import { useQuery } from '@tanstack/react-query';

import { requireSelectedNamespace, useNamespace } from '@/features/namespace';
import { ApiClient, type WorkflowPageRequest } from '@/lib/api';
import type { Namespace, WorkflowFilter } from '@/types';

const defaultApiClient = new ApiClient();

export type WorkflowQueryOptions = {
  apiClient?: Pick<ApiClient, 'queryWorkflows'>;
  filter: WorkflowFilter;
  page?: WorkflowPageRequest;
};

export function workflowListQueryKey(
  namespace: Namespace | null,
  filter: WorkflowFilter,
  page: WorkflowPageRequest = {}
) {
  return ['workflows', namespace, filter, page] as const;
}

export const workflowQueryKey = workflowListQueryKey;

export function queryWorkflowPage(
  apiClient: Pick<ApiClient, 'queryWorkflows'>,
  namespace: Namespace,
  filter: WorkflowFilter,
  page: WorkflowPageRequest = {}
) {
  return apiClient.queryWorkflows(filter, page, workflowQueryRequestOptions(namespace));
}

export function requireWorkflowQueryNamespace(namespace: Namespace | null | undefined): Namespace {
  return requireSelectedNamespace(namespace, 'querying workflows');
}

export function workflowQueryRequestOptions(namespace: Namespace | null | undefined) {
  return { namespace: requireWorkflowQueryNamespace(namespace) };
}

export function useWorkflowQuery({
  apiClient = defaultApiClient,
  filter,
  page = {},
}: WorkflowQueryOptions) {
  const { selectedNamespace } = useNamespace();

  return useQuery({
    enabled: selectedNamespace !== null && selectedNamespace.trim().length > 0,
    queryKey: workflowListQueryKey(selectedNamespace, filter, page),
    queryFn: () =>
      apiClient.queryWorkflows(filter, page, workflowQueryRequestOptions(selectedNamespace)),
  });
}
