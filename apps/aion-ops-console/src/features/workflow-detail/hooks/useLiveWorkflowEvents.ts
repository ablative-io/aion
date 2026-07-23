import { useQueryClient } from '@tanstack/react-query';
import { useMemo, useRef, useState } from 'react';

import { type EventSubscriptionManager, useBatchedEventSubscription } from '@/features/live-feed';
import { isSelectedNamespace, useNamespace } from '@/features/namespace';
import type { ResyncHandler, WorkflowEventSubscriptionFilter } from '@/lib/api';
import type { Event, WorkflowId } from '@/types';
import {
  eventSequence,
  isTerminalWorkflowEvent,
  mergeEventsBySequence,
  projectTimeline,
  terminalOutcomeForMergedEvents,
} from '../lib/timeline';
import type { LifecycleOutcome, TimelineEntry } from '../types';
import { workflowHistoryQueryKey } from './useWorkflowHistory';

export type LiveWorkflowEventsOptions = {
  enabled?: boolean;
  history: readonly Event[];
  manager?: EventSubscriptionManager;
  onResync?: ResyncHandler;
  workflowId: WorkflowId;
};

export type LiveWorkflowEvents = {
  events: Event[];
  isTerminal: boolean;
  lastSeenSequence: number | undefined;
  terminalOutcome: LifecycleOutcome | null;
  timeline: TimelineEntry[];
};

export function useLiveWorkflowEvents({
  enabled = true,
  history,
  manager,
  onResync,
  workflowId,
}: LiveWorkflowEventsOptions): LiveWorkflowEvents {
  const { selectedNamespace } = useNamespace();
  const queryClient = useQueryClient();
  const resetKey = `${selectedNamespace ?? ''}:${workflowId}:${history.length}`;
  const [liveEvents, setLiveEvents] = useState<Event[]>([]);
  const lastResetKey = useRef(resetKey);

  if (lastResetKey.current !== resetKey) {
    lastResetKey.current = resetKey;
    setLiveEvents([]);
  }

  const mergedEvents = useMemo(
    () => mergeEventsBySequence(history, liveEvents),
    [history, liveEvents]
  );
  const terminalOutcome = useMemo(
    () => terminalOutcomeForMergedEvents(mergedEvents),
    [mergedEvents]
  );
  const lastSeenSequence = useMemo(() => latestSequence(mergedEvents), [mergedEvents]);
  const timeline = useMemo(() => projectTimeline(mergedEvents), [mergedEvents]);
  const subscriptionFilter = useMemo<WorkflowEventSubscriptionFilter | null>(() => {
    if (!enabled || !isSelectedNamespace(selectedNamespace) || terminalOutcome !== null) {
      return null;
    }

    return { kind: 'workflow', namespace: selectedNamespace, workflowId };
  }, [enabled, selectedNamespace, terminalOutcome, workflowId]);

  useBatchedEventSubscription({
    enabled: subscriptionFilter !== null,
    filter: subscriptionFilter,
    lastSeenSequence,
    manager,
    flushAfter: isTerminalWorkflowEvent,
    onEvents: (events) => {
      setLiveEvents((current) => mergeEventsBySequence(current, events));
    },
    onResync: async (context) => {
      if (isSelectedNamespace(selectedNamespace)) {
        const queryKey = workflowHistoryQueryKey(selectedNamespace, workflowId);
        const cancelStaleQuery = () => {
          void queryClient.cancelQueries({ queryKey });
        };
        context.signal.addEventListener('abort', cancelStaleQuery, { once: true });

        try {
          await queryClient.invalidateQueries({ queryKey }, { throwOnError: true });
        } finally {
          context.signal.removeEventListener('abort', cancelStaleQuery);
        }
      }

      if (!context.isCurrent()) {
        return;
      }

      await onResync?.(context);
    },
  });

  return {
    events: mergedEvents,
    isTerminal: terminalOutcome !== null,
    lastSeenSequence,
    terminalOutcome,
    timeline,
  };
}

function latestSequence(events: readonly Event[]): number | undefined {
  return events.reduce<number | undefined>((latest, event) => {
    const sequence = eventSequence(event);
    return latest === undefined || sequence > latest ? sequence : latest;
  }, undefined);
}
