import { expect, test } from 'bun:test';

import type { Event as AionEvent } from '@/types';

import { createAionEventWebSocketManager } from './websocket';
import { FakeScheduler, type FakeSocket, FakeSocketFactory } from './websocket-test-support';

const NAMESPACE = 'default';
const WORKFLOW_ID = '00000000-0000-0000-0000-000000000001';
const EVENT: AionEvent = {
  type: 'WorkflowStarted',
  data: {
    envelope: {
      seq: 7,
      recorded_at: '2026-06-05T20:00:00Z',
      workflow_id: WORKFLOW_ID,
    },
    workflow_type: 'checkout',
    input: { content_type: 'Json', bytes: [123, 125] },
    run_id: '00000000-0000-0000-0000-0000000000a1',
    parent_run_id: null,
    package_version: '1.0.0',
  },
};

test('decode errors flush queued subscription delivery before publishing the error', () => {
  const socketFactory = new FakeSocketFactory();
  const transitions: string[] = [];
  let queued = true;
  const manager = createAionEventWebSocketManager({
    webSocketImpl: socketFactory.ctor,
    warn: () => undefined,
  });
  manager.onError((error) => {
    if (error !== null) {
      transitions.push('error');
    }
  });
  manager.subscribe(
    { kind: 'workflow', namespace: NAMESPACE, workflowId: WORKFLOW_ID },
    () => true,
    {
      flushPending: () => {
        if (queued) {
          queued = false;
          transitions.push('flush');
        }
      },
    }
  );
  const socket = socketFactory.sockets[0] as FakeSocket;
  socket.open();

  socket.message('{not-json');

  expect(transitions).toEqual(['flush', 'error']);
});

test('socket close flushes queued subscription delivery before disconnect state', () => {
  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  const transitions: string[] = [];
  let queued = false;
  const manager = createAionEventWebSocketManager({
    webSocketImpl: socketFactory.ctor,
    scheduler,
  });
  manager.onStatusChange((status) => transitions.push(status));
  manager.subscribe(
    { kind: 'workflow', namespace: NAMESPACE, workflowId: WORKFLOW_ID },
    () => {
      queued = true;
      return true;
    },
    {
      flushPending: () => {
        if (queued) {
          queued = false;
          transitions.push('flush');
        }
      },
    }
  );
  const socket = socketFactory.sockets[0] as FakeSocket;
  socket.open();
  socket.message(JSON.stringify({ namespace: NAMESPACE, event: EVENT }));

  socket.drop();

  expect(transitions).toEqual(['connected', 'flush', 'reconnecting']);
});
