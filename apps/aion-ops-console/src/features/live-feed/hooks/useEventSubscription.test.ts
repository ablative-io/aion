import { expect, test } from 'bun:test';
import { createAionEventWebSocketManager, type ResyncHandler } from '@/lib/api/websocket';
import {
  FakeScheduler,
  type FakeSocket,
  FakeSocketFactory,
} from '@/lib/api/websocket-test-support';
import type { Event as AionEvent } from '@/types';

import { resyncHandlerFromRef, subscribeToNamespaceFilter } from './useEventSubscription';

const namespace = 'default';
const event: AionEvent = {
  type: 'WorkflowStarted',
  data: {
    envelope: {
      seq: 7,
      recorded_at: '2026-06-05T20:00:00Z',
      workflow_id: '00000000-0000-0000-0000-000000000001',
    },
    workflow_type: 'checkout',
    input: { content_type: 'Json', bytes: [123, 125] },
    run_id: '00000000-0000-0000-0000-0000000000a1',
    parent_run_id: null,
    package_version: '1.0.0',
  },
};

test('adapter preserves absent firehose refetch and cannot clear a malformed-frame boundary', () => {
  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  const resyncRef: { current: ResyncHandler | undefined } = { current: undefined };
  let applications = 0;
  const manager = createAionEventWebSocketManager({
    webSocketImpl: socketFactory.ctor,
    scheduler,
    reconnect: { initialDelayMs: 10, maxAttempts: 1 },
    warn: () => undefined,
  });

  subscribeToNamespaceFilter(
    manager,
    namespace,
    { kind: 'firehose' },
    () => {
      applications += 1;
      return true;
    },
    { onResync: resyncHandlerFromRef(resyncRef) }
  );
  const firstSocket = socketFactory.sockets[0] as FakeSocket;
  firstSocket.open();
  firstSocket.message('{not-json');
  scheduler.runNext();
  const recoverySocket = socketFactory.sockets[1] as FakeSocket;
  recoverySocket.open();
  recoverySocket.message(JSON.stringify({ namespace, event }));

  expect(manager.getStatus()).toBe('resynced-with-possible-gap');
  expect(manager.getLastError()?.kind).toBe('frame-decode');
  expect(applications).toBe(1);
  expect(scheduler.delays).toEqual([10]);
});

test('adapter rejects recovery if a previously supplied refetch callback disappears', () => {
  const ref: { current: ResyncHandler | undefined } = { current: () => undefined };
  const handler = resyncHandlerFromRef(ref);
  ref.current = undefined;

  expect(handler).toBeDefined();
  expect(() =>
    handler?.({
      subscriptionId: 'aion-events-1',
      namespace,
      filter: { kind: 'firehose', namespace },
      lastSeenSequence: null,
      mode: 'full-refetch',
      generation: 1,
      signal: new AbortController().signal,
      isCurrent: () => true,
    })
  ).toThrow('refetch callback became unavailable');
});
