import { useCallback, useEffect, useMemo, useState } from 'react';

import type { ApiClient, AttemptCapabilities } from '@/lib/api';
import { createConfiguredApiClient } from '@/lib/config';
import type { Namespace, WorkflowId } from '@/types';

/**
 * Live intervenable-attempts hook (NOI-7).
 *
 * Loads the workflow's LIVE intervenable activity attempts + their advertised
 * capabilities from `POST /workflows/attempts`. The console reads this to pick a
 * transcript/intervention target and to gate controls: it renders ONLY the
 * primitives each attempt's owning worker advertises (an empty set = no controls).
 *
 * This is a discrete, user-triggered REST read — NOT a poll. It loads once per
 * `(namespace, workflow)` and exposes an explicit `refresh` the panel calls when
 * the operator asks to re-enumerate (e.g. after a new attempt starts, observed via
 * the live transcript/workflow socket). There is no `setInterval` anywhere.
 * Errors are surfaced as visible state, never swallowed; an empty list is the
 * honest answer for a workflow with no live agent attempt.
 */

export type AttemptsLoadState = 'idle' | 'loading' | 'ready' | 'error';

export type UseActivityAttemptsOptions = {
  /** The workflow to enumerate; `null` leaves the hook idle. */
  workflowId: WorkflowId | null;
  /** The namespace scope; `null` leaves the hook idle (no scope to authorize). */
  namespace: Namespace | null;
  /** Override the REST client (tests); defaults to the namespace-scoped client. */
  apiClient?: Pick<ApiClient, 'listAttempts'>;
};

export type UseActivityAttemptsResult = {
  /** The live intervenable attempts, ascending by activity then attempt. */
  attempts: AttemptCapabilities[];
  loadState: AttemptsLoadState;
  loadError: Error | null;
  /** Re-enumerate the live attempts (user-triggered; not a poll). */
  refresh: () => void;
};

/** Stable ascending sort by activityId then attempt (the list order). */
export function sortAttempts(attempts: AttemptCapabilities[]): AttemptCapabilities[] {
  return [...attempts].sort(
    (left, right) => left.activityId - right.activityId || left.attempt - right.attempt
  );
}

export function useActivityAttempts(
  options: UseActivityAttemptsOptions
): UseActivityAttemptsResult {
  const { workflowId, namespace } = options;
  const [attempts, setAttempts] = useState<AttemptCapabilities[]>([]);
  const [loadState, setLoadState] = useState<AttemptsLoadState>('idle');
  const [loadError, setLoadError] = useState<Error | null>(null);
  // A nonce the refresh callback bumps to re-run the load effect on demand.
  const [nonce, setNonce] = useState(0);

  const refresh = useCallback(() => {
    setNonce((current) => current + 1);
  }, []);

  // `nonce` is an intentional dependency: bumping it via `refresh()` re-runs the
  // load so the operator can re-enumerate live attempts on demand (never a poll).
  // biome-ignore lint/correctness/useExhaustiveDependencies: `nonce` is a deliberate refetch trigger, not read in the body.
  useEffect(() => {
    if (workflowId === null || namespace === null) {
      setAttempts([]);
      setLoadState('idle');
      setLoadError(null);
      return;
    }

    let active = true;
    const apiClient = options.apiClient ?? createConfiguredApiClient({ namespace });
    setLoadState('loading');

    apiClient
      .listAttempts(workflowId, { namespace })
      .then((rows) => {
        if (!active) {
          return;
        }
        setAttempts(sortAttempts(rows));
        setLoadState('ready');
        setLoadError(null);
      })
      .catch((error: unknown) => {
        if (!active) {
          return;
        }
        setLoadState('error');
        setLoadError(error instanceof Error ? error : new Error(String(error)));
      });

    return () => {
      active = false;
    };
  }, [workflowId, namespace, nonce, options.apiClient]);

  return useMemo(
    () => ({ attempts, loadState, loadError, refresh }),
    [attempts, loadState, loadError, refresh]
  );
}
