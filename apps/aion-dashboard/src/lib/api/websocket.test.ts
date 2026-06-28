import { expect, test } from 'bun:test';

import type { Event as AionEvent } from '@/types';

import {
  type AionSocketError,
  createAionEventWebSocketManager,
  type ResyncContext,
} from './websocket';

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

test('subscribe sends a per-workflow request and dispatches streamed typed events', () => {
  const socketFactory = new FakeSocketFactory();
  const manager = createAionEventWebSocketManager({
    baseUrl: 'https://aion.example',
    webSocketImpl: socketFactory.ctor,
  });
  const received: AionEvent[] = [];

  const unsubscribe = manager.subscribe({ kind: 'workflow', namespace, workflowId }, (nextEvent) =>
    received.push(nextEvent)
  );
  const socket = socketFactory.sockets[0] as FakeSocket;
  socket.open();
  socket.message(JSON.stringify({ namespace, event }));

  expect(socket.url).toBe('wss://aion.example/events/stream');
  expect(socket.sent.map((message) => JSON.parse(message) as unknown)).toEqual([
    {
      type: 'subscribe',
      subscription_id: 'aion-events-1',
      subscription: {
        per_workflow: {
          namespace,
          workflow_id: workflowId,
        },
      },
    },
  ]);
  expect(received).toEqual([event]);

  unsubscribe();
  expect(JSON.parse(socket.sent[1] ?? '{}')).toEqual({
    type: 'unsubscribe',
    subscription_id: 'aion-events-1',
  });
});

test('streamed wire-envelope payload bytes are decoded before dispatch', () => {
  const socketFactory = new FakeSocketFactory();
  const manager = createAionEventWebSocketManager({ webSocketImpl: socketFactory.ctor });
  const received: AionEvent[] = [];

  manager.subscribe({ kind: 'firehose', namespace }, (nextEvent) => received.push(nextEvent));
  const socket = socketFactory.sockets[0] as FakeSocket;
  socket.open();
  socket.message(
    JSON.stringify({
      namespace,
      event: {
        payload: {
          bytes: Array.from(new TextEncoder().encode(JSON.stringify(event))),
        },
      },
    })
  );

  expect(received).toEqual([event]);
});

test('malformed frames are logged instead of thrown into subscribers', () => {
  const socketFactory = new FakeSocketFactory();
  const warnings: unknown[][] = [];
  const manager = createAionEventWebSocketManager({
    webSocketImpl: socketFactory.ctor,
    warn: (...args) => warnings.push(args),
  });

  manager.subscribe({ kind: 'workflow', namespace, workflowId }, () => {
    throw new Error('handler should not run');
  });
  const socket = socketFactory.sockets[0] as FakeSocket;
  socket.open();
  socket.message('{not-json');

  expect(warnings).toHaveLength(1);
});

test('frames without namespace are logged instead of guessed from workflow id', () => {
  const socketFactory = new FakeSocketFactory();
  const warnings: unknown[][] = [];
  const received: AionEvent[] = [];
  const manager = createAionEventWebSocketManager({
    webSocketImpl: socketFactory.ctor,
    warn: (...args) => warnings.push(args),
  });

  manager.subscribe({ kind: 'workflow', namespace, workflowId }, (nextEvent) => {
    received.push(nextEvent);
  });
  const socket = socketFactory.sockets[0] as FakeSocket;
  socket.open();
  socket.message(JSON.stringify({ event }));

  expect(received).toEqual([]);
  expect(warnings).toHaveLength(1);
});

test('unexpected close reconnects with bounded backoff, re-sends subscriptions, and resyncs', () => {
  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  const manager = createAionEventWebSocketManager({
    webSocketImpl: socketFactory.ctor,
    scheduler,
    reconnect: { initialDelayMs: 10, maxDelayMs: 100, maxAttempts: 2 },
  });
  const statuses: string[] = [];
  const resyncs: ResyncContext[] = [];
  manager.onStatusChange((status) => statuses.push(status));

  manager.subscribe({ kind: 'workflow', namespace, workflowId }, () => undefined, {
    lastSeenSequence: 41,
    onResync: (context) => resyncs.push(context),
  });
  const firstSocket = socketFactory.sockets[0] as FakeSocket;
  firstSocket.open();
  firstSocket.drop();

  expect(statuses).toEqual(['connected', 'reconnecting']);
  expect(scheduler.delays).toEqual([10]);

  scheduler.runNext();
  const secondSocket = socketFactory.sockets[1] as FakeSocket;
  secondSocket.open();

  expect(statuses).toEqual(['connected', 'reconnecting', 'connected']);
  expect(JSON.parse(secondSocket.sent[0] ?? '{}')).toEqual({
    type: 'subscribe',
    subscription_id: 'aion-events-1',
    subscription: {
      per_workflow: {
        namespace,
        workflow_id: workflowId,
        after_seq: 41,
      },
    },
  });
  expect(resyncs).toEqual([
    {
      subscriptionId: 'aion-events-1',
      namespace,
      filter: { kind: 'workflow', namespace, workflowId },
      lastSeenSequence: 41,
      mode: 'after-sequence',
    },
  ]);
});

test('malformed frame emits a typed decode error to listeners (no silent failure)', () => {
  const socketFactory = new FakeSocketFactory();
  const errors: (AionSocketError | null)[] = [];
  const manager = createAionEventWebSocketManager({
    webSocketImpl: socketFactory.ctor,
    warn: () => undefined,
  });
  manager.onError((error) => errors.push(error));

  manager.subscribe({ kind: 'workflow', namespace, workflowId }, () => undefined);
  const socket = socketFactory.sockets[0] as FakeSocket;
  socket.open();
  socket.message('{not-json');

  expect(errors).toHaveLength(1);
  expect(errors[0]?.kind).toBe('frame-decode');
  expect(manager.getLastError()?.kind).toBe('frame-decode');
});

test('reconnect exhaustion emits a typed error then a healthy reconnect clears it', () => {
  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  const errors: (AionSocketError | null)[] = [];
  const manager = createAionEventWebSocketManager({
    webSocketImpl: socketFactory.ctor,
    scheduler,
    reconnect: { initialDelayMs: 10, maxAttempts: 1 },
  });
  manager.onError((error) => errors.push(error));

  manager.subscribe({ kind: 'workflow', namespace, workflowId }, () => undefined);
  (socketFactory.sockets[0] as FakeSocket).open();
  (socketFactory.sockets[0] as FakeSocket).drop();
  scheduler.runNext();
  (socketFactory.sockets[1] as FakeSocket).drop();

  expect(errors.map((error) => error?.kind ?? null)).toEqual(['reconnect-exhausted']);
  expect(manager.getLastError()?.kind).toBe('reconnect-exhausted');

  manager.connect();
  (socketFactory.sockets[2] as FakeSocket).open();

  expect(errors.at(-1)).toBeNull();
  expect(manager.getLastError()).toBeNull();
});

test('a single subscription sends exactly one subscribe frame (no double-subscribe)', () => {
  const socketFactory = new FakeSocketFactory();
  const manager = createAionEventWebSocketManager({ webSocketImpl: socketFactory.ctor });

  manager.subscribe({ kind: 'firehose', namespace }, () => undefined);
  const socket = socketFactory.sockets[0] as FakeSocket;
  socket.open();

  const subscribeFrames = socket.sent
    .map((message) => JSON.parse(message) as { type?: string })
    .filter((frame) => frame.type === 'subscribe');

  expect(subscribeFrames).toHaveLength(1);
  expect(socketFactory.sockets).toHaveLength(1);
});

test('socket error schedules a single reconnect attempt', () => {
  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  const manager = createAionEventWebSocketManager({
    webSocketImpl: socketFactory.ctor,
    scheduler,
    reconnect: { initialDelayMs: 10, maxAttempts: 3 },
  });
  const disconnects: string[] = [];
  manager.onDisconnect(() => disconnects.push('disconnected'));

  manager.subscribe({ kind: 'workflow', namespace, workflowId }, () => undefined);
  const socket = socketFactory.sockets[0] as FakeSocket;
  socket.open();
  socket.error();

  expect(disconnects).toEqual(['disconnected']);
  expect(scheduler.delays).toEqual([10]);
});

test('manual connect after bounded reconnect exhaustion starts a fresh attempt', () => {
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
  manager.connect();

  expect(socketFactory.sockets).toHaveLength(3);
});

test('filtered subscriptions only dispatch matching namespace and workflow filters', () => {
  const socketFactory = new FakeSocketFactory();
  const manager = createAionEventWebSocketManager({ webSocketImpl: socketFactory.ctor });
  const received: AionEvent[] = [];
  const otherNamespaceEvent: AionEvent = {
    ...event,
    data: {
      ...event.data,
      envelope: { ...event.data.envelope, workflow_id: otherWorkflowId },
    },
  };

  manager.subscribe(
    { kind: 'filtered', namespace, workflowType: 'checkout', status: 'Running' },
    (nextEvent) => received.push(nextEvent)
  );
  const socket = socketFactory.sockets[0] as FakeSocket;
  socket.open();
  socket.message(JSON.stringify({ namespace: 'other', event }));
  socket.message(JSON.stringify({ namespace, event: otherNamespaceEvent }));
  socket.message(JSON.stringify({ namespace, event }));

  expect(received).toEqual([otherNamespaceEvent, event]);
});

class FakeSocketFactory {
  readonly sockets: FakeSocket[] = [];
  readonly ctor: new (
    url: string
  ) => FakeSocket;

  constructor() {
    const sockets = this.sockets;

    this.ctor = class extends FakeSocket {
      constructor(url: string) {
        super(url);
        sockets.push(this);
      }
    };
  }
}

class FakeSocket {
  readonly sent: string[] = [];
  readyState = 0;
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onclose: ((event: { wasClean: boolean }) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;

  constructor(readonly url: string) {}

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.readyState = 3;
    this.onclose?.({ wasClean: true });
  }

  open(): void {
    this.readyState = 1;
    this.onopen?.({} as Event);
  }

  message(data: unknown): void {
    this.onmessage?.({ data });
  }

  drop(): void {
    this.readyState = 3;
    this.onclose?.({ wasClean: false });
  }

  error(): void {
    this.readyState = 3;
    this.onerror?.({} as Event);
  }
}

class FakeScheduler {
  readonly delays: number[] = [];
  private readonly callbacks: Array<() => void> = [];

  setTimeout(callback: () => void, delayMs: number): ReturnType<typeof setTimeout> {
    this.delays.push(delayMs);
    this.callbacks.push(callback);
    return this.callbacks.length as unknown as ReturnType<typeof setTimeout>;
  }

  clearTimeout(): void {
    this.callbacks.shift();
  }

  runNext(): void {
    this.callbacks.shift()?.();
  }
}
