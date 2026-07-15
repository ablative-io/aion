import { useEffect } from 'react';

import type { WorkflowId } from '@/types';

import { useLiveWorkflowEvents } from '../hooks/useLiveWorkflowEvents';
import { useWorkflowHistory } from '../hooks/useWorkflowHistory';
import type { ChildTimelineState } from './laneTree';

export type ChildTimelineLoaderProps = {
  path: string;
  workflowId: WorkflowId;
  onState: (path: string, state: ChildTimelineState) => void;
};

/**
 * Mounted only for an expanded child path. The durable history backfill completes
 * before the live tail attaches; unmounting on collapse closes that subscription.
 */
export function ChildTimelineLoader({ path, workflowId, onState }: ChildTimelineLoaderProps) {
  const historyQuery = useWorkflowHistory({ workflowId });
  const live = useLiveWorkflowEvents({
    enabled: historyQuery.isSuccess,
    history: historyQuery.data ?? [],
    workflowId,
  });

  useEffect(() => {
    if (historyQuery.isError) {
      onState(path, {
        status: 'error',
        entries: [],
        isRunning: false,
        message: errorMessage(historyQuery.error),
      });
      return;
    }
    if (!historyQuery.isSuccess) {
      onState(path, { status: 'loading', entries: [], isRunning: true });
      return;
    }
    onState(path, {
      status: 'ready',
      entries: live.timeline,
      isRunning: !live.isTerminal,
    });
  }, [
    historyQuery.error,
    historyQuery.isError,
    historyQuery.isSuccess,
    live.isTerminal,
    live.timeline,
    onState,
    path,
  ]);

  return null;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : 'Could not load child workflow history.';
}
