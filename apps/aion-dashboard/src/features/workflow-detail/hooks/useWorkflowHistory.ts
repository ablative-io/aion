import { useQuery } from '@tanstack/react-query';

import { createApiClient } from '@/lib/api';
import type { Namespace, WorkflowId } from '@/types';

export const workflowHistoryKey = (namespace: Namespace | null, workflowId: WorkflowId | null) =>
  ['aion-workflow-history', namespace, workflowId] as const;

export function useWorkflowHistory(namespace: Namespace | null, workflowId: WorkflowId | null) {
  return useQuery({
    queryKey: workflowHistoryKey(namespace, workflowId),
    queryFn: () => {
      if (namespace === null || workflowId === null) {
        throw new Error('Workflow history requires a namespace and workflow id.');
      }

      return createApiClient().getHistory(workflowId, { namespace });
    },
    enabled: namespace !== null && workflowId !== null,
  });
}
