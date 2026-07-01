import { useCallback, useState } from 'react';

import type { ApiClient, ReopenWorkflowResult } from '@/lib/api';
import { createConfiguredApiClient } from '@/lib/config';
import type { Namespace, RunId, WorkflowId } from '@/types';

/**
 * Reopen submit hook: POSTs a terminal (Failed/Cancelled) workflow to
 * `POST /workflows/reopen` and surfaces the server's typed result HONESTLY.
 *
 * A successful reopen returns the reopened run + its projected status (Running);
 * a rejected reopen (non-reopenable terminal, absent workflow, authorization)
 * surfaces as `error` (visible state) — never swallowed and never reinterpreted
 * as success. The live event stream is the source of truth for the workflow's
 * subsequent status transition, so this hook only reports the immediate outcome.
 */

export type ReopenSubmitState = 'idle' | 'submitting' | 'settled' | 'error';

export type UseReopenOptions = {
  /** The namespace scope for the command (the auth scope). */
  namespace: Namespace | null;
  /** Override the REST client (tests); defaults to the namespace-scoped client. */
  apiClient?: Pick<ApiClient, 'reopen'>;
};

export type UseReopenResult = {
  /** Submit a reopen; resolves to the typed result (or throws on error). */
  submit: (workflowId: WorkflowId, runId?: RunId) => Promise<ReopenWorkflowResult | null>;
  submitState: ReopenSubmitState;
  /** The last successful reopen result (reopened run + projected status). */
  lastResult: ReopenWorkflowResult | null;
  /** The last transport/authorization/invalid-state error. */
  error: Error | null;
};

export function useReopen(options: UseReopenOptions): UseReopenResult {
  const { namespace } = options;
  const [submitState, setSubmitState] = useState<ReopenSubmitState>('idle');
  const [lastResult, setLastResult] = useState<ReopenWorkflowResult | null>(null);
  const [error, setError] = useState<Error | null>(null);

  const submit = useCallback(
    async (workflowId: WorkflowId, runId?: RunId): Promise<ReopenWorkflowResult | null> => {
      if (namespace === null) {
        const missing = new Error('cannot reopen without a namespace scope');
        setSubmitState('error');
        setError(missing);
        setLastResult(null);
        return null;
      }

      const apiClient = options.apiClient ?? createConfiguredApiClient({ namespace });
      setSubmitState('submitting');
      setError(null);
      try {
        const result = await apiClient.reopen(workflowId, { namespace }, runId);
        setLastResult(result);
        setSubmitState('settled');
        return result;
      } catch (caught: unknown) {
        const asError = caught instanceof Error ? caught : new Error(String(caught));
        setError(asError);
        setSubmitState('error');
        setLastResult(null);
        throw asError;
      }
    },
    [namespace, options.apiClient]
  );

  return { submit, submitState, lastResult, error };
}
