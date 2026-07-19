import { expect, test } from 'bun:test';

import type { Event as AionEvent } from '@/types';

import {
  type AionSocketError,
  createAionEventWebSocketManager,
  type ResyncContext,
} from './websocket';
import {
  FakeScheduler,
  type FakeSocket,
  FakeSocketFactory,
} from './websocket-test-support';

const namespace = 'default';
const workflowId = '00000000-0000-0000-0000-000000000001';
const otherWorkflowId = '00000000-0000-0000-0000-000000000002';

const event: AionEvent = {
  type: 'WorkflowStarted',
  data: {
    envelope: {
      seq: 7,
      recorded_at: '2026-06-05T20:00:00Z',
      workflow_id: workflowId,
    },
    workflow_type: 'checkout',
    input: {
      content_type: 'Json',
      bytes: [123, 125],
    },
    run_id: '00000000-0000-0000-0000-0000000000a1',
    parent_run_id: null,
    package_version: '1.0.0',
  },
};

test('throwing durable subscriber reconnects from the last successfully applied sequence', () => {
  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  const errors: (AionSocketError | null)[] = [];
  const resyncs: ResyncContext[] = [];
  let applications = 0;
  const manager = createAionEventWebSocketManager({
    webSocketImpl: socketFactory.ctor,
    scheduler,
    reconnect: { initialDelayMs: 10, maxAttempts: 2 },
    warn: () => undefined,
  });
  manager.onError((error) => errors.push(error));

  manager.subscribe(
    { kind: 'workflow', namespace, workflowId },
    () => {
      applications += 1;
      if (applications === 1) {
        throw new Error('transient cache application failure');
      }
    },
    {
      lastSeenSequence: 6,
      onResync: (context) => resyncs.push(context),
    }
  );
  const firstSocket = socketFactory.sockets[0] as FakeSocket;
  firstSocket.open();
  firstSocket.message(JSON.stringify({ namespace, event }));

  expect(applications).toBe(1);
  expect(errors[0]?.kind).toBe('subscriber-application');
  expect(errors[0]?.subscriptionId).toBe('aion-events-1');
  expect(firstSocket.closeCalls).toBe(1);
  expect(scheduler.delays).toEqual([10]);

  // The failed socket is paused: even an already-queued later frame cannot run.
  firstSocket.message(JSON.stringify({ namespace, event }));
  expect(applications).toBe(1);

  scheduler.runNext();
  const secondSocket = socketFactory.sockets[1] as FakeSocket;
  secondSocket.open();

  expect(JSON.parse(secondSocket.sent[0] ?? '{}')).toEqual({
    per_workflow: {
      namespace,
      workflow_id: workflowId,
      resume_from_seq: 7,
    },
  });
  expect(resyncs[0]?.lastSeenSequence).toBe(6);
  expect(manager.getLastError()?.kind).toBe('subscriber-application');

  // Once replay applies successfully, the next reconnect advances past seq 7.
  secondSocket.message(JSON.stringify({ namespace, event }));
  expect(applications).toBe(2);
  expect(manager.getLastError()).toBeNull();
  secondSocket.drop();
  scheduler.runNext();
  const thirdSocket = socketFactory.sockets[2] as FakeSocket;
  thirdSocket.open();
  expect(JSON.parse(thirdSocket.sent[0] ?? '{}').per_workflow.resume_from_seq).toBe(8);
});

test('throwing live-only subscriber reconnects and requests a full refetch', () => {
  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  const resyncs: ResyncContext[] = [];
  const manager = createAionEventWebSocketManager({
    webSocketImpl: socketFactory.ctor,
    scheduler,
    reconnect: { initialDelayMs: 10, maxAttempts: 1 },
    warn: () => undefined,
  });

  manager.subscribe(
    { kind: 'filtered', namespace, workflowType: 'checkout' },
    () => {
      throw new Error('transient live projection failure');
    },
    { onResync: (context) => resyncs.push(context) }
  );
  (socketFactory.sockets[0] as FakeSocket).open();
  (socketFactory.sockets[0] as FakeSocket).message(JSON.stringify({ namespace, event }));
  scheduler.runNext();
  (socketFactory.sockets[1] as FakeSocket).open();

  expect(resyncs.map((context) => context.mode)).toEqual(['full-refetch']);
});

test('recovering one subscription cannot clear another subscription error', () => {
  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  const errors: (AionSocketError | null)[] = [];
  const manager = createAionEventWebSocketManager({
    webSocketImpl: socketFactory.ctor,
    scheduler,
    reconnect: { initialDelayMs: 10, maxAttempts: 1 },
    warn: () => undefined,
  });
  manager.onError((error) => errors.push(error));

  manager.subscribe({ kind: 'workflow', namespace, workflowId }, () => undefined);
  manager.subscribe(
    { kind: 'workflow', namespace, workflowId: otherWorkflowId },
    () => undefined
  );
  const firstSocket = socketFactory.sockets[0] as FakeSocket;
  const secondSocket = socketFactory.sockets[1] as FakeSocket;
  firstSocket.open();
  secondSocket.open();

  firstSocket.drop();
  scheduler.runNext();
  (socketFactory.sockets[2] as FakeSocket).drop();
  expect(manager.getLastError()).toMatchObject({
    kind: 'reconnect-exhausted',
    subscriptionId: 'aion-events-1',
  });

  // B has its own dropped-frame error. Recovering B must reveal A's older,
  // still-active exhausted error rather than reporting the manager as healthy.
  secondSocket.message('{not-json');
  expect(manager.getLastError()).toMatchObject({
    kind: 'frame-decode',
    subscriptionId: 'aion-events-2',
  });
  secondSocket.drop();
  scheduler.runNext();
  (socketFactory.sockets[3] as FakeSocket).open();

  expect(manager.getStatus()).toBe('disconnected');
  expect(manager.getLastError()).toMatchObject({
    kind: 'reconnect-exhausted',
    subscriptionId: 'aion-events-1',
  });
  expect(errors.at(-1)).not.toBeNull();
});

test('adding a subscription does not resurrect an exhausted subscription', () => {
  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  const manager = createAionEventWebSocketManager({
    webSocketImpl: socketFactory.ctor,
    scheduler,
    reconnect: { initialDelayMs: 10, maxAttempts: 1 },
  });

  manager.subscribe({ kind: 'workflow', namespace, workflowId }, () => undefined);
  (socketFactory.sockets[0] as FakeSocket).open();
  (socketFactory.sockets[0] as FakeSocket).drop();
  scheduler.runNext();
  (socketFactory.sockets[1] as FakeSocket).drop();

  manager.subscribe(
    { kind: 'workflow', namespace, workflowId: otherWorkflowId },
    () => undefined
  );

  // Exactly one new socket belongs to the new subscription. A remains stopped
  // at its max-attempt bound until an explicit manager.connect() retry.
  expect(socketFactory.sockets).toHaveLength(3);
  const newSubscriptionSocket = socketFactory.sockets[2] as FakeSocket;
  newSubscriptionSocket.open();
  expect(JSON.parse(newSubscriptionSocket.sent[0] ?? '{}')).toEqual({
    per_workflow: { namespace, workflow_id: otherWorkflowId },
  });
  expect(manager.getLastError()?.subscriptionId).toBe('aion-events-1');
});
