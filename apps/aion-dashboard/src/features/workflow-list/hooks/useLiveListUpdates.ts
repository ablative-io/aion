import { useMemo } from 'react';
import { useQueryClient } from '@tanstack/react-query';

import { isSelectedNamespace, useNamespace } from '@/features/namespace';
import type { EventSubscriptionManager } from '@/features/live-feed';
import { useEventSubscription } from '@/features/live-feed';
import type { FilteredEventSubscriptionFilter, WorkflowPage, WorkflowPageRequest } from '@/lib/api';
import type { Event, WorkflowFilter, WorkflowStatus, WorkflowSummary } from '@/types';
import { workflowListQueryKey } from './useWorkflowQuery';

export type LiveListUpdatesOptions = {
  filter: WorkflowFilter;
  manager?: EventSubscriptionManager;
  page?: WorkflowPageRequest;
};

export function useLiveListUpdates({ filter, manager, page = {} }: LiveListUpdatesOptions) {
  const { selectedNamespace } = useNamespace();
  const queryClient = useQueryClient();
  const queryKey = useMemo(
    () => workflowListQueryKey(selectedNamespace, filter, page),
    [filter, page, selectedNamespace]
  );
  const subscriptionFilter = useMemo<FilteredEventSubscriptionFilter | null>(() => {
    if (!isSelectedNamespace(selectedNamespace)) {
      return null;
    }

    return {
      kind: 'filtered',
      namespace: selectedNamespace,
      status: filter.status,
      workflowType: filter.workflow_type,
    };
  }, [filter.status, filter.workflow_type, selectedNamespace]);

  return useEventSubscription({
    enabled: subscriptionFilter !== null,
    filter: subscriptionFilter,
    manager,
    onEvent: (event) => {
      if (!isSelectedNamespace(selectedNamespace)) {
        return;
      }

      const patched = patchWorkflowPage(queryClient.getQueryData(queryKey), event, filter, page.limit);

      if (patched === null) {
        void queryClient.invalidateQueries({ queryKey });
        return;
      }

      queryClient.setQueryData(queryKey, patched);
    },
    onResync: () => {
      if (isSelectedNamespace(selectedNamespace)) {
        void queryClient.invalidateQueries({ queryKey });
      }
    },
  });
}

export function patchWorkflowPage(
  page: WorkflowPage<WorkflowSummary> | undefined,
  event: Event,
  filter: WorkflowFilter,
  limit?: number
): WorkflowPage<WorkflowSummary> | null {
  if (page === undefined) {
    return null;
  }

  const workflowId = event.data.envelope.workflow_id;
  const existingIndex = page.items.findIndex((item) => item.workflow_id === workflowId);

  if (existingIndex >= 0) {
    const updated = patchExistingSummary(page.items[existingIndex], event);

    if (updated === null) {
      return page;
    }

    const items = [...page.items];
    items[existingIndex] = updated;
    return { ...page, items };
  }

  const inserted = summaryFromStartedEvent(event);

  if (inserted === null || !summaryMatchesFilter(inserted, filter)) {
    return null;
  }

  const items = [inserted, ...page.items];
  const normalizedLimit = normalizeLimit(limit);
  return {
    ...page,
    hasMore: page.hasMore || items.length > normalizedLimit,
    items: items.slice(0, normalizedLimit),
  };
}

function patchExistingSummary(summary: WorkflowSummary, event: Event): WorkflowSummary | null {
  const nextStatus = statusFromEvent(event);

  if (nextStatus === null && event.type !== 'WorkflowStarted') {
    return null;
  }

  if (event.type === 'WorkflowStarted') {
    return {
      ...summary,
      started_at: event.data.envelope.recorded_at,
      status: 'Running',
      workflow_type: event.data.workflow_type,
    };
  }

  if (nextStatus === null) {
    return null;
  }

  return {
    ...summary,
    ended_at: isTerminalStatus(nextStatus) ? event.data.envelope.recorded_at : summary.ended_at,
    status: nextStatus,
  };
}

function summaryFromStartedEvent(event: Event): WorkflowSummary | null {
  if (event.type !== 'WorkflowStarted') {
    return null;
  }

  return {
    workflow_id: event.data.envelope.workflow_id,
    workflow_type: event.data.workflow_type,
    status: 'Running',
    started_at: event.data.envelope.recorded_at,
    ended_at: null,
    parent: null,
  };
}

function statusFromEvent(event: Event): WorkflowStatus | null {
  switch (event.type) {
    case 'WorkflowStarted':
      return 'Running';
    case 'WorkflowCompleted':
      return 'Completed';
    case 'WorkflowFailed':
      return 'Failed';
    case 'WorkflowCancelled':
      return 'Cancelled';
    case 'WorkflowTimedOut':
      return 'TimedOut';
    default:
      return null;
  }
}

function summaryMatchesFilter(summary: WorkflowSummary, filter: WorkflowFilter): boolean {
  if (filter.workflow_type !== null && summary.workflow_type !== filter.workflow_type) {
    return false;
  }

  if (filter.status !== null && summary.status !== filter.status) {
    return false;
  }

  if (filter.started_after !== null && summary.started_at < filter.started_after) {
    return false;
  }

  if (filter.started_before !== null && summary.started_at > filter.started_before) {
    return false;
  }

  return filter.parent === null || summary.parent === filter.parent;
}

function isTerminalStatus(status: WorkflowStatus): boolean {
  return status !== 'Running';
}

function normalizeLimit(limit: number | undefined): number {
  if (limit === undefined || !Number.isFinite(limit)) {
    return 50;
  }

  return Math.max(1, Math.floor(limit));
}
