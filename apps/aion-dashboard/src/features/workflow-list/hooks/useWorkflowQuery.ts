import { useQuery } from '@tanstack/react-query';

import { useNamespace } from '@/features/namespace';
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

export function requireWorkflowQueryNamespace(namespace: Namespace | null): Namespace {
  if (namespace === null) {
    throw new Error('A namespace must be selected before querying workflows.');
  }

  return namespace;
}

export function workflowQueryRequestOptions(namespace: Namespace | null) {
  return { namespace: requireWorkflowQueryNamespace(namespace) };
}

export function useWorkflowQuery({
  apiClient = defaultApiClient,
  filter,
  page = {},
}: WorkflowQueryOptions) {
  const { selectedNamespace } = useNamespace();

  return useQuery({
    enabled: selectedNamespace !== null,
    queryKey: workflowListQueryKey(selectedNamespace, filter, page),
    queryFn: () =>
      apiClient.queryWorkflows(filter, page, workflowQueryRequestOptions(selectedNamespace)),
  });
}
