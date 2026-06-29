import { useQuery } from '@tanstack/react-query';
import { useMemo } from 'react';

import { requireSelectedNamespace, useNamespace } from '@/features/namespace';
import type { ApiClient, WorkflowPageRequest } from '@/lib/api';
import { createConfiguredApiClient } from '@/lib/config';
import type { Namespace, WorkflowFilter } from '@/types';

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

export function useWorkflowQuery({ apiClient, filter, page = {} }: WorkflowQueryOptions) {
  const { selectedNamespace } = useNamespace();
  const client = useMemo<Pick<ApiClient, 'queryWorkflows'>>(
    () => apiClient ?? createConfiguredApiClient({ namespace: selectedNamespace }),
    [apiClient, selectedNamespace]
  );

  return useQuery({
    enabled: selectedNamespace !== null && selectedNamespace.trim().length > 0,
    queryKey: workflowListQueryKey(selectedNamespace, filter, page),
    queryFn: () =>
      queryWorkflowPage(client, requireWorkflowQueryNamespace(selectedNamespace), filter, page),
  });
}
