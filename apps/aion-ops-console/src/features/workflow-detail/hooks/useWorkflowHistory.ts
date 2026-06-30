import { useQuery } from '@tanstack/react-query';
import { useMemo } from 'react';

import { requireSelectedNamespace, useNamespace } from '@/features/namespace';
import type { ApiClient } from '@/lib/api';
import { createConfiguredApiClient } from '@/lib/config';
import type { Namespace, WorkflowId } from '@/types';

export type WorkflowHistoryOptions = {
  apiClient?: Pick<ApiClient, 'getHistory'>;
  workflowId: WorkflowId;
};

export function workflowHistoryQueryKey(namespace: Namespace | null, workflowId: WorkflowId) {
  return ['workflow-history', namespace, workflowId] as const;
}

export function requireWorkflowHistoryNamespace(
  namespace: Namespace | null | undefined
): Namespace {
  return requireSelectedNamespace(namespace, 'loading workflow history');
}

export function workflowHistoryRequestOptions(namespace: Namespace | null | undefined) {
  return { namespace: requireWorkflowHistoryNamespace(namespace) };
}

export function useWorkflowHistory({ apiClient, workflowId }: WorkflowHistoryOptions) {
  const { selectedNamespace } = useNamespace();
  const client = useMemo<Pick<ApiClient, 'getHistory'>>(
    () => apiClient ?? createConfiguredApiClient({ namespace: selectedNamespace }),
    [apiClient, selectedNamespace]
  );

  return useQuery({
    enabled: selectedNamespace !== null && selectedNamespace.trim().length > 0,
    queryKey: workflowHistoryQueryKey(selectedNamespace, workflowId),
    queryFn: () => client.getHistory(workflowId, workflowHistoryRequestOptions(selectedNamespace)),
  });
}
