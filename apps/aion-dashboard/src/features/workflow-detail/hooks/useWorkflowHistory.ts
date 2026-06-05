import { useQuery } from '@tanstack/react-query';

import { useNamespace } from '@/features/namespace';
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

export function requireWorkflowHistoryNamespace(namespace: Namespace | null): Namespace {
  if (namespace === null) {
    throw new Error('A namespace must be selected before loading workflow history.');
  }

  return namespace;
}

export function workflowHistoryRequestOptions(namespace: Namespace | null) {
  return { namespace: requireWorkflowHistoryNamespace(namespace) };
}

export function useWorkflowHistory({
  apiClient = defaultApiClient,
  workflowId,
}: WorkflowHistoryOptions) {
  const { selectedNamespace } = useNamespace();

  return useQuery({
    enabled: selectedNamespace !== null,
    queryKey: workflowHistoryQueryKey(selectedNamespace, workflowId),
    queryFn: () =>
      apiClient.getHistory(workflowId, workflowHistoryRequestOptions(selectedNamespace)),
  });
}
