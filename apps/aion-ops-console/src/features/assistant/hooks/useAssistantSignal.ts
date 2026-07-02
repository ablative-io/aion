import { useCallback, useState } from 'react';

import type { ApiClient, JsonRecord } from '@/lib/api';
import { createConfiguredApiClient } from '@/lib/config';
import type { Namespace, WorkflowId } from '@/types';

/**
 * Signal-submit hook for the assistant session (`POST /workflows/signal`).
 * Mirrors {@link useIntervene}'s honest state surface: a transport/authorization
 * failure is visible `error` state, never swallowed; the signal ack body is
 * empty, so `settled` means the server confirmed delivery (2xx), nothing more.
 */

export type AssistantSignalState = 'idle' | 'submitting' | 'settled' | 'error';

export type UseAssistantSignalOptions = {
  namespace: Namespace | null;
  workflowId: WorkflowId;
  /** Override the REST client (tests); defaults to the namespace-scoped client. */
  apiClient?: Pick<ApiClient, 'sendSignal'>;
};

export type UseAssistantSignalResult = {
  submit: (signalName: string, payload?: JsonRecord) => Promise<void>;
  submitState: AssistantSignalState;
  error: Error | null;
};

export function useAssistantSignal(options: UseAssistantSignalOptions): UseAssistantSignalResult {
  const { namespace, workflowId } = options;
  const [submitState, setSubmitState] = useState<AssistantSignalState>('idle');
  const [error, setError] = useState<Error | null>(null);

  const submit = useCallback(
    async (signalName: string, payload?: JsonRecord): Promise<void> => {
      if (namespace === null) {
        const missing = new Error('cannot signal without a namespace scope');
        setSubmitState('error');
        setError(missing);
        return;
      }

      const apiClient = options.apiClient ?? createConfiguredApiClient({ namespace });
      setSubmitState('submitting');
      setError(null);
      try {
        await apiClient.sendSignal(workflowId, signalName, { namespace }, payload);
        setSubmitState('settled');
      } catch (caught: unknown) {
        const asError = caught instanceof Error ? caught : new Error(String(caught));
        setError(asError);
        setSubmitState('error');
        throw asError;
      }
    },
    [namespace, workflowId, options.apiClient]
  );

  return { submit, submitState, error };
}
