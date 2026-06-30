import { expect, mock, test } from 'bun:test';

import { createAionEventWebSocketManager } from '@/lib/api';

mock.module('react', () => ({
  useSyncExternalStore: (
    subscribe: (notify: () => void) => () => void,
    getSnapshot: () => string
  ) => {
    const cleanup = subscribe(() => undefined);
    cleanup();
    return getSnapshot();
  },
}));

const namespace = 'default';
const workflowId = '00000000-0000-0000-0000-000000000001';

test('useConnectionStatus is exported as a hook function', async () => {
  const { useConnectionStatus } = await import('./useConnectionStatus');

  expect(typeof useConnectionStatus).toBe('function');
  expect(useConnectionStatus()).toBe('disconnected');
});

test('manager status store reports connect, drop, and reconnect sequence consumed by the hook', () => {
  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  const manager = createAionEventWebSocketManager({
    webSocketImpl: socketFactory.ctor,
    scheduler,
    reconnect: { initialDelayMs: 5, maxAttempts: 2 },
  });
  const statuses = [manager.getStatus()];
  const cleanup = manager.onStatusChange(() => {
    statuses.push(manager.getStatus());
  });
  const afterCleanupStatuses: string[] = [];

  manager.subscribe({ kind: 'workflow', namespace, workflowId }, () => undefined);
  const firstSocket = socketFactory.sockets[0] as FakeSocket;
  firstSocket.open();
  firstSocket.drop();
  scheduler.runNext();
  const secondSocket = socketFactory.sockets[1] as FakeSocket;
  secondSocket.open();
  cleanup();
  manager.onStatusChange((status) => afterCleanupStatuses.push(status));
  secondSocket.drop();

  expect(statuses).toEqual(['disconnected', 'connected', 'reconnecting', 'connected']);
  expect(afterCleanupStatuses).toEqual(['reconnecting']);
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
  readyState = 0;
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onclose: ((event: { wasClean: boolean }) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;

  constructor(readonly url: string) {}

  send(): void {}

  close(): void {
    this.readyState = 3;
    this.onclose?.({ wasClean: true });
  }

  open(): void {
    this.readyState = 1;
    this.onopen?.({} as Event);
  }

  drop(): void {
    this.readyState = 3;
    this.onclose?.({ wasClean: false });
  }
}

class FakeScheduler {
  private readonly callbacks: Array<() => void> = [];

  setTimeout(callback: () => void): ReturnType<typeof setTimeout> {
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
