import { useQuery } from '@tanstack/react-query';

import { requireSelectedNamespace, useNamespace } from '@/features/namespace';
import { ApiClient } from '@/lib/api';
import type { Namespace, WorkflowId } from '@/types';

const defaultApiClient = new ApiClient();

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

export function useWorkflowHistory({
  apiClient = defaultApiClient,
  workflowId,
}: WorkflowHistoryOptions) {
  const { selectedNamespace } = useNamespace();

  return useQuery({
    enabled: selectedNamespace !== null && selectedNamespace.trim().length > 0,
    queryKey: workflowHistoryQueryKey(selectedNamespace, workflowId),
    queryFn: () =>
      apiClient.getHistory(workflowId, workflowHistoryRequestOptions(selectedNamespace)),
  });
}
