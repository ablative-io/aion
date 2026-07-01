import { useCallback, useState } from 'react';

import type { ApiClient, InterveneParams } from '@/lib/api';
import { createConfiguredApiClient } from '@/lib/config';
import type { InterventionOutcome, Namespace } from '@/types';

/**
 * Mid-run intervention submit hook (NOI-7).
 *
 * POSTs a neutral intervention command to `POST /workflows/intervene` and surfaces
 * the server's neutral {@link InterventionOutcome} ack HONESTLY — `Applied`,
 * `CapabilityNotSupported`, or `StaleTarget` are ALL returned verbatim to the
 * caller as the last outcome, never swallowed and never reinterpreted as success.
 * A transport/authorization error is surfaced as `error` (visible state), distinct
 * from a first-class gated/stale ack.
 */

export type InterveneSubmitState = 'idle' | 'submitting' | 'settled' | 'error';

export type UseInterveneOptions = {
  /** The namespace scope for the command (the auth scope). */
  namespace: Namespace | null;
  /** Override the REST client (tests); defaults to the namespace-scoped client. */
  apiClient?: Pick<ApiClient, 'intervene'>;
};

export type UseInterveneResult = {
  /** Submit a command; resolves to the neutral outcome (or throws on transport error). */
  submit: (params: InterveneParams) => Promise<InterventionOutcome | null>;
  submitState: InterveneSubmitState;
  /** The last neutral ack (applied / gated / stale), surfaced verbatim. */
  lastOutcome: InterventionOutcome | null;
  /** The last transport/authorization error (distinct from a first-class ack). */
  error: Error | null;
};

export function useIntervene(options: UseInterveneOptions): UseInterveneResult {
  const { namespace } = options;
  const [submitState, setSubmitState] = useState<InterveneSubmitState>('idle');
  const [lastOutcome, setLastOutcome] = useState<InterventionOutcome | null>(null);
  const [error, setError] = useState<Error | null>(null);

  const submit = useCallback(
    async (params: InterveneParams): Promise<InterventionOutcome | null> => {
      if (namespace === null) {
        const missing = new Error('cannot intervene without a namespace scope');
        setSubmitState('error');
        setError(missing);
        setLastOutcome(null);
        return null;
      }

      const apiClient = options.apiClient ?? createConfiguredApiClient({ namespace });
      setSubmitState('submitting');
      setError(null);
      try {
        const outcome = await apiClient.intervene(params, { namespace });
        // The ack is surfaced verbatim — a gated or stale outcome is NOT an error
        // and is NOT reinterpreted as success.
        setLastOutcome(outcome);
        setSubmitState('settled');
        return outcome;
      } catch (caught: unknown) {
        const asError = caught instanceof Error ? caught : new Error(String(caught));
        setError(asError);
        setSubmitState('error');
        setLastOutcome(null);
        throw asError;
      }
    },
    [namespace, options.apiClient]
  );

  return { submit, submitState, lastOutcome, error };
}
