import { useCallback, useEffect, useMemo, useRef, useState } from 'react';

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

/**
 * `refreshing` is a re-enumeration of the SAME (namespace, workflow) after a
 * successful load: the previous attempts stay valid state (consumers keep
 * rendering them) rather than flapping back through `loading`.
 */
export type AttemptsLoadState = 'idle' | 'loading' | 'refreshing' | 'ready' | 'error';

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
  // The identity the current attempts belong to; a change means the previous
  // rows are another workflow's and must NOT survive as `refreshing` state.
  const identityRef = useRef<string | null>(null);

  const refresh = useCallback(() => {
    setNonce((current) => current + 1);
  }, []);

  // `nonce` is an intentional dependency: bumping it via `refresh()` re-runs the
  // load so the operator can re-enumerate live attempts on demand (never a poll).
  // biome-ignore lint/correctness/useExhaustiveDependencies: `nonce` is a deliberate refetch trigger, not read in the body.
  useEffect(() => {
    if (workflowId === null || namespace === null) {
      identityRef.current = null;
      setAttempts([]);
      setLoadState('idle');
      setLoadError(null);
      return;
    }

    let active = true;
    const apiClient = options.apiClient ?? createConfiguredApiClient({ namespace });
    const identity = `${namespace}:${workflowId}`;
    if (identityRef.current === identity) {
      // Same target re-enumerating: keep the previous rows on screen and only
      // step to `refreshing` when they were actually loaded.
      setLoadState((current) =>
        current === 'ready' || current === 'refreshing' ? 'refreshing' : 'loading'
      );
    } else {
      identityRef.current = identity;
      setAttempts([]);
      setLoadState('loading');
    }

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
