import { expect, test } from 'bun:test';
import { QueryClient, QueryObserver } from '@tanstack/react-query';

import {
  affectsWorkflowSummaryProjection,
  patchWorkflowPage,
  refetchWorkflowListForRecovery,
} from '@/features/workflow-list/hooks/useLiveListUpdates';
import type { Event, WorkflowFilter, WorkflowSummary } from '@/types';

import { createAionEventWebSocketManager, type ResyncContext } from './websocket';
import {
  deferred,
  FakeScheduler,
  type FakeSocket,
  FakeSocketFactory,
  flushMicrotasks,
} from './websocket-test-support';

const namespace = 'default';
const workflowId = '00000000-0000-0000-0000-000000000001';
const event: Event = {
  type: 'WorkflowStarted',
  data: {
    envelope: {
      seq: 7,
      recorded_at: '2026-06-05T20:00:00Z',
      workflow_id: workflowId,
    },
    workflow_type: 'checkout',
    input: { content_type: 'Json', bytes: [123, 125] },
    run_id: '00000000-0000-0000-0000-0000000000a1',
    parent_run_id: null,
    package_version: '1.0.0',
  },
};

function eventAt(sequence: number): Event {
  return {
    ...event,
    data: { ...event.data, envelope: { ...event.data.envelope, seq: sequence } },
  } as Event;
}

function activityEventAt(sequence: number): Event {
  return {
    type: 'ActivityStarted',
    data: {
      envelope: {
        seq: sequence,
        recorded_at: '2026-06-05T20:01:00Z',
        workflow_id: workflowId,
      },
      activity_id: 1,
      attempt: 1,
    },
  };
}

test('dirty list refetch repeats before clearing the gap after the inverse cache race', async () => {
  type WorkflowPage = {
    items: WorkflowSummary[];
    nextCursor: string | null;
    hasMore: boolean;
  };

  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const queryKey = ['workflows', namespace, 'inverse-race'] as const;
  const emptySnapshot: WorkflowPage = { items: [], nextCursor: null, hasMore: false };
  const firstRefetch = deferred<WorkflowPage>();
  const secondRefetch = deferred<WorkflowPage>();
  let refetchCalls = 0;
  queryClient.setQueryData(queryKey, emptySnapshot);
  const observer = new QueryObserver(queryClient, {
    queryKey,
    queryFn: () => {
      refetchCalls += 1;
      return refetchCalls === 1 ? firstRefetch.promise : secondRefetch.promise;
    },
    staleTime: Number.POSITIVE_INFINITY,
  });
  const stopObserver = observer.subscribe(() => undefined);
  const filter: WorkflowFilter = {
    workflow_type: 'checkout',
    status: 'Running',
    started_after: null,
    started_before: null,
    parent: null,
  };
  const manager = createAionEventWebSocketManager({
    webSocketImpl: socketFactory.ctor,
    scheduler,
    reconnect: { initialDelayMs: 10, maxAttempts: 2 },
    warn: () => undefined,
  });

  manager.subscribe(
    { kind: 'filtered', namespace, workflowType: 'checkout', status: 'Running' },
    (liveEvent) => {
      const current = queryClient.getQueryData<WorkflowPage>(queryKey);
      const patched = patchWorkflowPage(current, liveEvent, filter);
      if (patched !== null) {
        queryClient.setQueryData(queryKey, patched);
      }
      return affectsWorkflowSummaryProjection(liveEvent);
    },
    {
      onResync: (context) => refetchWorkflowListForRecovery(queryClient, queryKey, context),
    }
  );
  (socketFactory.sockets[0] as FakeSocket).open();
  (socketFactory.sockets[0] as FakeSocket).drop();
  scheduler.runNext();
  const recoverySocket = socketFactory.sockets[1] as FakeSocket;
  recoverySocket.open();

  expect(refetchCalls).toBe(1);
  expect(manager.getStatus()).toBe('resynced-with-possible-gap');

  recoverySocket.message(JSON.stringify({ namespace, event }));
  const liveProjection = queryClient.getQueryData<WorkflowPage>(queryKey);
  expect(liveProjection?.items.map((item) => item.workflow_id)).toEqual([workflowId]);

  // The request read its empty Running snapshot before WorkflowStarted arrived.
  // Its late response overwrites the live cache patch, so this dirty completion
  // cannot prove current state and must start another bounded refetch.
  firstRefetch.resolve(emptySnapshot);
  await flushMicrotasks();
  expect(refetchCalls).toBe(2);
  expect(queryClient.getQueryData<WorkflowPage>(queryKey)?.items).toEqual([]);
  expect(manager.getStatus()).toBe('resynced-with-possible-gap');

  secondRefetch.resolve(liveProjection as WorkflowPage);
  await flushMicrotasks();
  expect(
    queryClient.getQueryData<WorkflowPage>(queryKey)?.items.map((item) => item.workflow_id)
  ).toEqual([workflowId]);
  expect(manager.getStatus()).toBe('connected');
  expect(manager.getLastError()).toBeNull();

  stopObserver();
  manager.reset();
});

test('projection-neutral activity frames do not dirty an authoritative list refetch', async () => {
  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  const refetch = deferred<void>();
  let refetchCalls = 0;
  const manager = createAionEventWebSocketManager({
    webSocketImpl: socketFactory.ctor,
    scheduler,
    reconnect: { initialDelayMs: 1, maxAttempts: 2 },
    warn: () => undefined,
  });

  manager.subscribe(
    { kind: 'filtered', namespace, workflowType: 'checkout', status: 'Running' },
    (liveEvent) => affectsWorkflowSummaryProjection(liveEvent),
    {
      onResync: () => {
        refetchCalls += 1;
        return refetch.promise;
      },
    }
  );
  (socketFactory.sockets[0] as FakeSocket).open();
  (socketFactory.sockets[0] as FakeSocket).drop();
  scheduler.runNext();
  const recoverySocket = socketFactory.sockets[1] as FakeSocket;
  recoverySocket.open();

  recoverySocket.message(JSON.stringify({ namespace, event: activityEventAt(8) }));
  recoverySocket.message(JSON.stringify({ namespace, event: activityEventAt(9) }));
  expect(refetchCalls).toBe(1);
  expect(manager.getStatus()).toBe('resynced-with-possible-gap');

  refetch.resolve();
  await flushMicrotasks();

  expect(refetchCalls).toBe(1);
  expect(recoverySocket.closeCalls).toBe(0);
  expect(manager.getStatus()).toBe('connected');
  expect(manager.getLastError()).toBeNull();
  expect(scheduler.pendingCount).toBe(0);
});

test('a timed-out recovery generation cannot commit after a newer refetch succeeds', async () => {
  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  const first = deferred<string>();
  const second = deferred<string>();
  const contexts: ResyncContext[] = [];
  let projection = 'seed';
  const manager = createAionEventWebSocketManager({
    webSocketImpl: socketFactory.ctor,
    scheduler,
    reconnect: { initialDelayMs: 1, maxAttempts: 2 },
    resyncTimeoutMs: 5,
    warn: () => undefined,
  });

  manager.subscribe({ kind: 'firehose', namespace }, () => undefined, {
    onResync: async (context) => {
      contexts.push(context);
      const next = await (contexts.length === 1 ? first.promise : second.promise);
      if (context.isCurrent()) {
        projection = next;
      }
    },
  });
  (socketFactory.sockets[0] as FakeSocket).open();
  (socketFactory.sockets[0] as FakeSocket).drop();
  scheduler.runNext();
  (socketFactory.sockets[1] as FakeSocket).open();
  scheduler.runNext();
  scheduler.runNext();
  (socketFactory.sockets[2] as FakeSocket).open();

  second.resolve('fresh');
  await flushMicrotasks();
  expect(manager.getStatus()).toBe('connected');
  expect(projection).toBe('fresh');
  expect(contexts[0]?.signal.aborted).toBeTrue();
  expect(contexts[0]?.isCurrent()).toBeFalse();
  expect(contexts[1]?.generation).toBeGreaterThan(contexts[0]?.generation ?? 0);

  first.resolve('stale');
  await flushMicrotasks();
  expect(manager.getStatus()).toBe('connected');
  expect(projection).toBe('fresh');
});

test('a continuously dirty refetch exhausts the shared recovery budget', async () => {
  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  const first = deferred<void>();
  const second = deferred<void>();
  let refetchCalls = 0;
  const manager = createAionEventWebSocketManager({
    webSocketImpl: socketFactory.ctor,
    scheduler,
    reconnect: { initialDelayMs: 1, maxAttempts: 2 },
    warn: () => undefined,
  });

  manager.subscribe(
    { kind: 'firehose', namespace },
    (liveEvent) => affectsWorkflowSummaryProjection(liveEvent),
    {
      onResync: () => {
        refetchCalls += 1;
        return refetchCalls === 1 ? first.promise : second.promise;
      },
    }
  );
  (socketFactory.sockets[0] as FakeSocket).open();
  (socketFactory.sockets[0] as FakeSocket).drop();
  scheduler.runNext();
  const recoverySocket = socketFactory.sockets[1] as FakeSocket;
  recoverySocket.open();
  recoverySocket.message(JSON.stringify({ namespace, event: eventAt(8) }));
  first.resolve();
  await flushMicrotasks();

  expect(refetchCalls).toBe(2);
  expect(manager.getStatus()).toBe('resynced-with-possible-gap');
  recoverySocket.message(JSON.stringify({ namespace, event: eventAt(9) }));
  second.resolve();
  await flushMicrotasks();

  expect(socketFactory.sockets).toHaveLength(2);
  expect(recoverySocket.closeCalls).toBe(1);
  expect(manager.getStatus()).toBe('disconnected');
  expect(manager.getLastError()).toMatchObject({
    kind: 'reconnect-exhausted',
    subscriptionId: 'aion-events-1',
  });
  expect(scheduler.pendingCount).toBe(0);
});
