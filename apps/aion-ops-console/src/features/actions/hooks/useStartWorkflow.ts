import { useMutation, useQueryClient } from '@tanstack/react-query';
import { useMemo } from 'react';

import { requireSelectedNamespace } from '@/features/namespace';
import type { ApiClient, StartWorkflowParams, StartWorkflowResult } from '@/lib/api';
import { createConfiguredApiClient } from '@/lib/config';
import type { Namespace } from '@/types';

export type StartWorkflowVariables = StartWorkflowParams;

export type UseStartWorkflowOptions = {
  namespace: Namespace | null;
  /** Injected client (tests); defaults to the configured, namespace-scoped client. */
  apiClient?: Pick<ApiClient, 'startWorkflow'> | undefined;
};

/**
 * Mutation that starts a workflow run against `POST /workflows/start`.
 *
 * The run ids in {@link StartWorkflowResult} exist only after the server confirms
 * the start — the mutation resolves with the real response, so the view shows
 * success only on a confirmed run, never optimistically. A server failure (404
 * `WorkflowTypeNotFound`, 403 `namespace_denied`, 400 invalid input) surfaces as
 * the mutation `error`, never swallowed. On success the workflow-list queries are
 * invalidated so the new run appears without a manual refresh.
 */
export function useStartWorkflow({ namespace, apiClient }: UseStartWorkflowOptions) {
  const queryClient = useQueryClient();
  const client = useMemo<Pick<ApiClient, 'startWorkflow'>>(
    () => apiClient ?? createConfiguredApiClient({ namespace }),
    [apiClient, namespace]
  );

  return useMutation<StartWorkflowResult, Error, StartWorkflowVariables>({
    mutationFn: (variables) =>
      client.startWorkflow(variables, {
        namespace: requireSelectedNamespace(namespace, 'starting a workflow'),
      }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['workflows'] });
    },
  });
}
