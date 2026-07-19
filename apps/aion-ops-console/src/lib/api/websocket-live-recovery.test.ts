import { expect, test } from 'bun:test';

import type { Event as AionEvent } from '@/types';

import {
  type AionSocketError,
  createAionEventWebSocketManager,
  type ResyncContext,
} from './websocket';
import { FakeScheduler, type FakeSocket, FakeSocketFactory } from './websocket-test-support';

const namespace = 'default';
const workflowId = '00000000-0000-0000-0000-000000000001';

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

function eventAt(sequence: number): AionEvent {
  return {
    ...event,
    data: { ...event.data, envelope: { ...event.data.envelope, seq: sequence } },
  } as AionEvent;
}

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
    {
      onResync: (context) => {
        resyncs.push(context);
      },
    }
  );
  (socketFactory.sockets[0] as FakeSocket).open();
  (socketFactory.sockets[0] as FakeSocket).message(JSON.stringify({ namespace, event }));
  scheduler.runNext();
  (socketFactory.sockets[1] as FakeSocket).open();

  expect(resyncs.map((context) => context.mode)).toEqual(['full-refetch']);
});

test('Running projection keeps a post-completion HTTP snapshot authoritative', async () => {
  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  let projection: string[] = [];
  let completedWhileRefetchPending = false;
  let resolveResync: (() => void) | undefined;
  const resync = new Promise<void>((resolve) => {
    resolveResync = () => {
      // The workflow completed while the Running-only HTTP request was pending.
      // Its terminal frame is selector-filtered, and the resulting snapshot is empty.
      completedWhileRefetchPending = true;
      projection = [];
      resolve();
    };
  });
  let applications = 0;
  const manager = createAionEventWebSocketManager({
    webSocketImpl: socketFactory.ctor,
    scheduler,
    reconnect: { initialDelayMs: 10, maxAttempts: 1 },
    warn: () => undefined,
  });

  manager.subscribe(
    { kind: 'filtered', namespace, workflowType: 'checkout', status: 'Running' },
    () => {
      applications += 1;
      if (applications === 1) {
        throw new Error('transient live projection failure');
      }
      projection = ['w1:Running'];
    },
    { onResync: () => resync }
  );
  (socketFactory.sockets[0] as FakeSocket).open();
  (socketFactory.sockets[0] as FakeSocket).message(JSON.stringify({ namespace, event }));
  scheduler.runNext();
  const recoverySocket = socketFactory.sockets[1] as FakeSocket;
  recoverySocket.open();
  recoverySocket.message(JSON.stringify({ namespace, event: eventAt(8) }));

  expect(manager.getLastError()?.kind).toBe('subscriber-application');
  expect(manager.getStatus()).toBe('resynced-with-possible-gap');
  expect(applications).toBe(2);
  expect(projection).toEqual(['w1:Running']);

  resolveResync?.();
  await resync;
  await Promise.resolve();

  expect(manager.getLastError()).toBeNull();
  expect(manager.getStatus()).toBe('connected');
  expect(completedWhileRefetchPending).toBeTrue();
  expect(applications).toBe(2);
  // The Running-only HTTP snapshot completed after the older WorkflowStarted
  // frame and remains authoritative. No post-snapshot buffer can reinsert w1.
  expect(projection).toEqual([]);
});

test('rejected live-only refetch stays visible and exhausts the bounded recovery', async () => {
  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  let rejectResync: ((cause: Error) => void) | undefined;
  const resync = new Promise<void>((_resolve, reject) => {
    rejectResync = reject;
  });
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
    { onResync: () => resync }
  );
  (socketFactory.sockets[0] as FakeSocket).open();
  (socketFactory.sockets[0] as FakeSocket).message(JSON.stringify({ namespace, event }));
  scheduler.runNext();
  const recoverySocket = socketFactory.sockets[1] as FakeSocket;
  recoverySocket.open();

  rejectResync?.(new Error('HTTP refetch failed'));
  await resync.catch(() => undefined);
  await Promise.resolve();

  expect(recoverySocket.closeCalls).toBe(1);
  expect(socketFactory.sockets).toHaveLength(2);
  expect(scheduler.delays).toEqual([10, 10_000]);
  expect(manager.getStatus()).toBe('disconnected');
  expect(manager.getLastError()).toMatchObject({
    kind: 'reconnect-exhausted',
    subscriptionId: 'aion-events-1',
  });
});

test('live-only reconnect without a refetch callback remains visibly possibly gapped', () => {
  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  const manager = createAionEventWebSocketManager({
    webSocketImpl: socketFactory.ctor,
    scheduler,
    reconnect: { initialDelayMs: 10, maxAttempts: 1 },
    warn: () => undefined,
  });

  manager.subscribe({ kind: 'firehose', namespace }, () => undefined);
  (socketFactory.sockets[0] as FakeSocket).open();
  (socketFactory.sockets[0] as FakeSocket).message('{not-json');
  scheduler.runNext();
  (socketFactory.sockets[1] as FakeSocket).open();

  expect(socketFactory.sockets).toHaveLength(2);
  expect(manager.getStatus()).toBe('resynced-with-possible-gap');
  expect(manager.getLastError()?.kind).toBe('frame-decode');
  expect(scheduler.delays).toEqual([10]);
});

test('a never-settling refetch has no frame buffer and times out into visible exhaustion', () => {
  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  const errors: (AionSocketError | null)[] = [];
  let applications = 0;
  const manager = createAionEventWebSocketManager({
    webSocketImpl: socketFactory.ctor,
    scheduler,
    reconnect: { initialDelayMs: 10, maxAttempts: 1 },
    resyncTimeoutMs: 25,
    warn: () => undefined,
  });
  manager.onError((error) => errors.push(error));

  manager.subscribe(
    { kind: 'firehose', namespace },
    () => {
      applications += 1;
    },
    { onResync: () => new Promise<void>(() => undefined) }
  );
  (socketFactory.sockets[0] as FakeSocket).open();
  (socketFactory.sockets[0] as FakeSocket).drop();
  scheduler.runNext();
  const recoverySocket = socketFactory.sockets[1] as FakeSocket;
  recoverySocket.open();

  for (let sequence = 1; sequence <= 10_000; sequence += 1) {
    recoverySocket.message(JSON.stringify({ namespace, event: eventAt(sequence) }));
  }

  // Frames are applied in arrival order instead of being retained behind the
  // hung HTTP request, so memory use is independent of a pending-frame queue.
  expect(applications).toBe(10_000);
  expect(manager.getStatus()).toBe('resynced-with-possible-gap');
  expect(scheduler.delays).toEqual([10, 25]);
  expect(scheduler.pendingCount).toBe(1);

  scheduler.runNext();

  expect(recoverySocket.closeCalls).toBe(1);
  expect(manager.getStatus()).toBe('disconnected');
  expect(errors.map((error) => error?.kind ?? null)).toEqual([
    'subscriber-resync',
    'reconnect-exhausted',
  ]);
  expect(errors[0]?.cause).toBeInstanceOf(Error);
  expect((errors[0]?.cause as Error).message).toContain('timed out after 25ms');
  expect(manager.getLastError()?.kind).toBe('reconnect-exhausted');
  expect(scheduler.pendingCount).toBe(0);

  // An explicit retry resets the budget, not the continuity claim. It must run
  // the refetch again and remain visibly gapped rather than clearing on open.
  manager.connect();
  const explicitRetrySocket = socketFactory.sockets[2] as FakeSocket;
  explicitRetrySocket.open();
  expect(manager.getStatus()).toBe('resynced-with-possible-gap');
  expect(manager.getLastError()?.kind).toBe('reconnect-exhausted');
  expect(scheduler.pendingCount).toBe(1);
  manager.close();
  expect(scheduler.pendingCount).toBe(0);
});
