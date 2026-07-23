import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';

import { requireSelectedNamespace, useNamespace } from '@/features/namespace';
import type { ApiClient, HistoryWindow, RequestOptions } from '@/lib/api';
import { createConfiguredApiClient } from '@/lib/config';
import type { Event, Namespace, WorkflowId } from '@/types';

import { mergeEventsBySequence } from '../lib/timeline';

export const HISTORY_WINDOW_SIZE = 500;

export type WorkflowHistoryClient = Pick<ApiClient, 'getHistoryWindow'>;

export type WorkflowHistorySnapshot = {
  events: Event[];
  headSeq: number;
  loadedFromSeq: number;
  nextFromSeq: number | null;
};

export type WorkflowHistoryOptions = {
  apiClient?: WorkflowHistoryClient;
  enabled?: boolean;
  workflowId: WorkflowId;
};

export function workflowHistoryQueryKey(namespace: Namespace | null, workflowId: WorkflowId) {
  return ['workflow-history', namespace, workflowId] as const;
}

export function requireWorkflowHistoryNamespace(
  namespace: Namespace | null | undefined
): Namespace {
  return requireSelectedNamespace(namespace, 'loading workflow history');
}

export function workflowHistoryRequestOptions(
  namespace: Namespace | null | undefined,
  signal?: AbortSignal
): RequestOptions {
  return {
    namespace: requireWorkflowHistoryNamespace(namespace),
    ...(signal !== undefined && { signal }),
  };
}

/** Head discovery is payload-cheap; the second request supplies terminal-first paint. */
export async function fetchInitialWorkflowHistory(
  client: WorkflowHistoryClient,
  workflowId: WorkflowId,
  options: RequestOptions
): Promise<WorkflowHistorySnapshot> {
  const discovery = await client.getHistoryWindow(workflowId, { fromSeq: 0, limit: 1 }, options);
  const loadedFromSeq = Math.max(0, discovery.headSeq + 1 - HISTORY_WINDOW_SIZE);
  const tail = await client.getHistoryWindow(
    workflowId,
    { fromSeq: loadedFromSeq, limit: HISTORY_WINDOW_SIZE },
    options
  );

  return snapshotFromWindow(tail, loadedFromSeq);
}

export async function fetchEarlierWorkflowHistory(
  client: WorkflowHistoryClient,
  workflowId: WorkflowId,
  current: WorkflowHistorySnapshot,
  options: RequestOptions
): Promise<WorkflowHistorySnapshot> {
  if (current.loadedFromSeq === 0) {
    return current;
  }
  const fromSeq = Math.max(0, current.loadedFromSeq - HISTORY_WINDOW_SIZE);
  const limit = current.loadedFromSeq - fromSeq;
  const page = await client.getHistoryWindow(workflowId, { fromSeq, limit }, options);

  return {
    events: mergeEventsBySequence(page.events, current.events),
    headSeq: Math.max(current.headSeq, page.headSeq),
    loadedFromSeq: fromSeq,
    nextFromSeq: current.nextFromSeq,
  };
}

export async function fetchNextWorkflowHistory(
  client: WorkflowHistoryClient,
  workflowId: WorkflowId,
  current: WorkflowHistorySnapshot,
  options: RequestOptions
): Promise<WorkflowHistorySnapshot> {
  if (current.nextFromSeq === null) {
    return current;
  }
  const requestedFromSeq = current.nextFromSeq;
  const page = await client.getHistoryWindow(
    workflowId,
    { fromSeq: requestedFromSeq, limit: HISTORY_WINDOW_SIZE },
    options
  );
  assertCursorAdvanced(requestedFromSeq, page.nextFromSeq);

  return {
    events: mergeEventsBySequence(current.events, page.events),
    headSeq: Math.max(current.headSeq, page.headSeq),
    loadedFromSeq: current.loadedFromSeq,
    nextFromSeq: page.nextFromSeq,
  };
}

export function useWorkflowHistory({
  apiClient,
  enabled = true,
  workflowId,
}: WorkflowHistoryOptions) {
  const { selectedNamespace } = useNamespace();
  const queryClient = useQueryClient();
  const client = useMemo<WorkflowHistoryClient>(
    () => apiClient ?? createConfiguredApiClient({ namespace: selectedNamespace }),
    [apiClient, selectedNamespace]
  );
  const queryKey = useMemo(
    () => workflowHistoryQueryKey(selectedNamespace, workflowId),
    [selectedNamespace, workflowId]
  );
  const [isFetchingEarlier, setIsFetchingEarlier] = useState(false);
  const [isFetchingForward, setIsFetchingForward] = useState(false);
  const [windowError, setWindowError] = useState<unknown>(null);
  const earlierAbort = useRef<AbortController | null>(null);

  const query = useQuery({
    enabled: enabled && selectedNamespace !== null && selectedNamespace.trim().length > 0,
    queryKey,
    queryFn: ({ signal }) =>
      fetchInitialWorkflowHistory(
        client,
        workflowId,
        workflowHistoryRequestOptions(selectedNamespace, signal)
      ),
  });

  useEffect(() => {
    const snapshot = query.data;
    if (snapshot?.nextFromSeq === null || snapshot === undefined) {
      return;
    }
    const expectedCursor = snapshot.nextFromSeq;
    const controller = new AbortController();
    setIsFetchingForward(true);
    void fetchNextWorkflowHistory(
      client,
      workflowId,
      snapshot,
      workflowHistoryRequestOptions(selectedNamespace, controller.signal)
    )
      .then((next) => {
        if (controller.signal.aborted) {
          return;
        }
        queryClient.setQueryData<WorkflowHistorySnapshot>(queryKey, (current) =>
          current?.nextFromSeq === expectedCursor ? next : current
        );
      })
      .catch((error: unknown) => {
        if (!controller.signal.aborted) {
          setWindowError(error);
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) {
          setIsFetchingForward(false);
        }
      });

    return () => controller.abort();
  }, [client, query.data, queryClient, queryKey, selectedNamespace, workflowId]);

  useEffect(() => () => earlierAbort.current?.abort(), []);

  const loadEarlier = useCallback(async () => {
    const snapshot = queryClient.getQueryData<WorkflowHistorySnapshot>(queryKey);
    if (snapshot === undefined || snapshot.loadedFromSeq === 0 || earlierAbort.current !== null) {
      return;
    }
    const controller = new AbortController();
    earlierAbort.current = controller;
    setIsFetchingEarlier(true);
    setWindowError(null);
    try {
      const next = await fetchEarlierWorkflowHistory(
        client,
        workflowId,
        snapshot,
        workflowHistoryRequestOptions(selectedNamespace, controller.signal)
      );
      if (!controller.signal.aborted) {
        queryClient.setQueryData(queryKey, next);
      }
    } catch (error) {
      if (!controller.signal.aborted) {
        setWindowError(error);
      }
    } finally {
      if (!controller.signal.aborted) {
        earlierAbort.current = null;
        setIsFetchingEarlier(false);
      }
    }
  }, [client, queryClient, queryKey, selectedNamespace, workflowId]);

  const refetch = useCallback(() => {
    setWindowError(null);
    return query.refetch();
  }, [query]);

  return {
    ...query,
    data: query.data?.events,
    error: windowError ?? query.error,
    hasEarlier: (query.data?.loadedFromSeq ?? 0) > 0,
    headSeq: query.data?.headSeq,
    isError: query.isError || windowError !== null,
    isFetchingEarlier,
    isFetchingForward,
    loadEarlier,
    refetch,
  };
}

function snapshotFromWindow(window: HistoryWindow, loadedFromSeq: number): WorkflowHistorySnapshot {
  return {
    events: window.events,
    headSeq: window.headSeq,
    loadedFromSeq,
    nextFromSeq: window.nextFromSeq,
  };
}

function assertCursorAdvanced(requested: number, next: number | null): void {
  if (next !== null && next <= requested) {
    throw new Error('workflows/history next_from_seq did not advance');
  }
}
