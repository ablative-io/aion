import { useEffect, useMemo, useState } from 'react';
import { useQueryClient } from '@tanstack/react-query';

import { useEventSubscription, type EventSubscriptionManager } from '@/features/live-feed';
import { isSelectedNamespace, useNamespace } from '@/features/namespace';
import type { WorkflowEventSubscriptionFilter } from '@/lib/api';
import type { Event, WorkflowId } from '@/types';
import {
  eventSequence,
  mergeEventsBySequence,
  projectTimeline,
  terminalOutcomeForEvents,
} from '../lib/timeline';
import type { LifecycleOutcome, TimelineEntry } from '../types';
import { workflowHistoryQueryKey } from './useWorkflowHistory';

export type LiveWorkflowEventsOptions = {
  history: readonly Event[];
  manager?: EventSubscriptionManager;
  onResync?: () => void;
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
  history,
  manager,
  onResync,
  workflowId,
}: LiveWorkflowEventsOptions): LiveWorkflowEvents {
  const { selectedNamespace } = useNamespace();
  const queryClient = useQueryClient();
  const [liveEvents, setLiveEvents] = useState<Event[]>([]);

  useEffect(() => {
    setLiveEvents([]);
  }, [history, selectedNamespace, workflowId]);

  const mergedEvents = useMemo(() => mergeEventsBySequence(history, liveEvents), [history, liveEvents]);
  const terminalOutcome = useMemo(() => terminalOutcomeForEvents(mergedEvents), [mergedEvents]);
  const lastSeenSequence = useMemo(() => latestSequence(mergedEvents), [mergedEvents]);
  const subscriptionFilter = useMemo<WorkflowEventSubscriptionFilter | null>(() => {
    if (!isSelectedNamespace(selectedNamespace) || terminalOutcome !== null) {
      return null;
    }

    return { kind: 'workflow', namespace: selectedNamespace, workflowId };
  }, [selectedNamespace, terminalOutcome, workflowId]);

  useEventSubscription({
    enabled: subscriptionFilter !== null,
    filter: subscriptionFilter,
    lastSeenSequence,
    manager,
    onEvent: (event) => {
      setLiveEvents((current) => mergeEventsBySequence(current, [event]));
    },
    onResync: () => {
      if (isSelectedNamespace(selectedNamespace)) {
        void queryClient.invalidateQueries({
          queryKey: workflowHistoryQueryKey(selectedNamespace, workflowId),
        });
      }

      onResync?.();
    },
  });

  return {
    events: mergedEvents,
    isTerminal: terminalOutcome !== null,
    lastSeenSequence,
    terminalOutcome,
    timeline: projectTimeline(mergedEvents),
  };
}

function latestSequence(events: readonly Event[]): number | undefined {
  return events.reduce<number | undefined>((latest, event) => {
    const sequence = eventSequence(event);
    return latest === undefined || sequence > latest ? sequence : latest;
  }, undefined);
}
