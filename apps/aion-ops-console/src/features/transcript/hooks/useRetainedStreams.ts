import { useEffect, useMemo, useState } from 'react';

import type { RetainedStreamHead, TranscriptReadClient } from '@/lib/api';
import { createConfiguredTranscriptReader } from '@/lib/config';
import type { Namespace, WorkflowId } from '@/types';

/**
 * Retained-transcript stream enumeration (lane #230, consuming the lane-#229
 * `POST /workflows/transcripts` endpoint).
 *
 * Loads once per `(namespace, workflowId)` identity — a discrete REST read, NOT
 * a poll. The attempt rows themselves stay timeline-derived (every started
 * attempt is durably enumerated by `ActivityStarted`); this enumeration only
 * badges which rows have a RETAINED durable transcript. An empty list is the
 * honest answer for a pre-retention run; a failure is visible state
 * (`loadError`), never a throw into render and never swallowed.
 */

export type UseRetainedStreamsOptions = {
  /** The workflow to enumerate; `null` leaves the hook idle. */
  workflowId: WorkflowId | null;
  /** The namespace scope; `null` leaves the hook idle (no scope to authorize). */
  namespace: Namespace | null;
  /** Override the REST reader (tests); defaults to the configured client. */
  reader?: Pick<TranscriptReadClient, 'listStreams'>;
};

export type UseRetainedStreamsResult = {
  /** The retained streams, as enumerated by the server (may be empty). */
  streams: RetainedStreamHead[];
  loadState: 'idle' | 'loading' | 'ready' | 'error';
  loadError: Error | null;
};

export function useRetainedStreams(options: UseRetainedStreamsOptions): UseRetainedStreamsResult {
  const { workflowId, namespace } = options;
  const [streams, setStreams] = useState<RetainedStreamHead[]>([]);
  const [loadState, setLoadState] = useState<UseRetainedStreamsResult['loadState']>('idle');
  const [loadError, setLoadError] = useState<Error | null>(null);

  useEffect(() => {
    if (workflowId === null || namespace === null) {
      setStreams([]);
      setLoadState('idle');
      setLoadError(null);
      return;
    }

    let active = true;
    const reader = options.reader ?? createConfiguredTranscriptReader({ namespace });
    setStreams([]);
    setLoadState('loading');
    setLoadError(null);

    reader
      .listStreams(namespace, workflowId)
      .then((rows) => {
        if (!active) {
          return;
        }
        setStreams(rows);
        setLoadState('ready');
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
  }, [workflowId, namespace, options.reader]);

  return useMemo(() => ({ streams, loadState, loadError }), [streams, loadState, loadError]);
}
