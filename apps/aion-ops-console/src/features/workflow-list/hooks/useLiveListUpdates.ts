import { useQueryClient } from '@tanstack/react-query';
import { useCallback, useMemo, useState } from 'react';
import type { EventSubscriptionManager, EventSubscriptionState } from '@/features/live-feed';
import { useEventSubscription } from '@/features/live-feed';
import { isSelectedNamespace, useNamespace } from '@/features/namespace';
import type { FilteredEventSubscriptionFilter, WorkflowPage, WorkflowPageRequest } from '@/lib/api';
import type { Event, WorkflowFilter, WorkflowStatus, WorkflowSummary } from '@/types';
import { workflowListQueryKey } from './useWorkflowQuery';

export type LiveListUpdatesOptions = {
  filter: WorkflowFilter;
  manager?: EventSubscriptionManager;
  page?: WorkflowPageRequest;
  /**
   * Whether the currently-viewed page is the first page (cursor absent). New
   * `WorkflowStarted` rows are only inserted into the visible page here; on a
   * deeper page inserting at index 0 would corrupt the cursor-anchored view
   * (risk register R3). Off page 1 the new row is NOT mutated into the page —
   * it is counted and surfaced as a "new above" affordance instead, so a fresh
   * workflow is never silently dropped from the operator's view.
   */
  isFirstPage?: boolean;
};

/**
 * Result of the live list subscription. Extends the bare subscription state with
 * the count of newly-started workflows observed while viewing a deeper page,
 * plus an explicit acknowledge action. The count drives a visible "N new above"
 * affordance — there is no silent suppression.
 */
export type LiveListUpdatesState = EventSubscriptionState & {
  newAboveCount: number;
  acknowledgeNewAbove: () => void;
};

export function useLiveListUpdates({
  filter,
  manager,
  page = {},
  isFirstPage = true,
}: LiveListUpdatesOptions): LiveListUpdatesState {
  const { selectedNamespace } = useNamespace();
  const queryClient = useQueryClient();
  const [newAboveCount, setNewAboveCount] = useState(0);
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

  const acknowledgeNewAbove = useCallback(() => {
    setNewAboveCount(0);
    if (isSelectedNamespace(selectedNamespace)) {
      void queryClient.invalidateQueries({ queryKey });
    }
  }, [queryClient, queryKey, selectedNamespace]);

  const onEvent = useCallback(
    (event: Event) => {
      if (!isSelectedNamespace(selectedNamespace)) {
        return;
      }

      const current = queryClient.getQueryData<WorkflowPage<WorkflowSummary>>(queryKey);
      const patched = patchWorkflowPage(current, event, filter, page.limit, {
        allowInsert: isFirstPage,
      });

      if (patched === null) {
        // Insert was suppressed because we are not on page 1: surface it as a
        // visible "new above" count rather than mutating the cursor view, but
        // only when the event truly represents a newly-started matching row.
        if (!isFirstPage && isNewMatchingStart(current, event, filter)) {
          setNewAboveCount((count) => count + 1);
          return;
        }

        void queryClient.invalidateQueries({ queryKey });
        return;
      }

      queryClient.setQueryData(queryKey, patched);
    },
    [filter, isFirstPage, page.limit, queryClient, queryKey, selectedNamespace]
  );

  const onResync = useCallback(async () => {
    if (isSelectedNamespace(selectedNamespace)) {
      setNewAboveCount(0);
      await queryClient.invalidateQueries({ queryKey }, { throwOnError: true });
    }
  }, [queryClient, queryKey, selectedNamespace]);

  const subscription = useEventSubscription({
    enabled: subscriptionFilter !== null,
    filter: subscriptionFilter,
    manager,
    onEvent,
    onResync,
  });

  return { ...subscription, newAboveCount, acknowledgeNewAbove };
}

/**
 * True when `event` is a `WorkflowStarted` for a workflow not already on the
 * page and matching the active filter — i.e. a row that WOULD have inserted at
 * the top on page 1. Used to count suppressed inserts off page 1.
 */
function isNewMatchingStart(
  page: WorkflowPage<WorkflowSummary> | undefined,
  event: Event,
  filter: WorkflowFilter
): boolean {
  if (page === undefined) {
    return false;
  }

  const inserted = summaryFromStartedEvent(event);

  if (inserted === null) {
    return false;
  }

  const alreadyPresent = page.items.some((item) => item.workflow_id === inserted.workflow_id);
  return !alreadyPresent && summaryMatchesFilter(inserted, filter);
}

export type PatchWorkflowPageOptions = {
  /**
   * When false, a newly-started matching workflow is NOT inserted into the page
   * (the caller surfaces it as "new above" instead). In-place status patches to
   * rows already on the page always apply regardless of this flag.
   */
  allowInsert?: boolean;
};

export function patchWorkflowPage(
  page: WorkflowPage<WorkflowSummary> | undefined,
  event: Event,
  filter: WorkflowFilter,
  limit?: number,
  options: PatchWorkflowPageOptions = {}
): WorkflowPage<WorkflowSummary> | null {
  if (page === undefined) {
    return null;
  }

  const { allowInsert = true } = options;
  const workflowId = event.data.envelope.workflow_id;
  const existingIndex = page.items.findIndex((item) => item.workflow_id === workflowId);
  const existing = existingIndex >= 0 ? page.items[existingIndex] : undefined;

  if (existing !== undefined) {
    const updated = patchExistingSummary(existing, event);

    if (updated === null) {
      return page;
    }

    const items = [...page.items];
    items[existingIndex] = updated;
    return { ...page, items };
  }

  if (!allowInsert) {
    return null;
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
