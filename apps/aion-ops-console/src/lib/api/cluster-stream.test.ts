import { expect, test } from 'bun:test';

import type { AionSocketError } from './websocket';
import { createAionClusterStreamManager } from './cluster-stream';
import { FakeScheduler, type FakeSocket, FakeSocketFactory } from './websocket-test-support';

function snapshot(node: string, asOfSeq: number) {
  return {
    kind: 'cluster_snapshot',
    snapshot: { node, as_of_seq: asOfSeq, peers: [], shards: [], workers: [] },
  };
}

test('reconnect stays visibly possibly gapped with stale topology until a fresh snapshot applies', () => {
  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  const topology: string[] = [];
  const manager = createAionClusterStreamManager({
    webSocketImpl: socketFactory.ctor,
    scheduler,
    reconnect: { initialDelayMs: 1, maxAttempts: 2 },
    resyncTimeoutMs: 25,
    warn: () => undefined,
  });

  manager.subscribe({
    onSnapshot: (value) => topology.push(value.node),
    onEvent: () => undefined,
  });
  const initialSocket = socketFactory.sockets[0] as FakeSocket;
  expect(manager.getStatus()).toBe('reconnecting');
  initialSocket.open();
  expect(manager.getStatus()).toBe('reconnecting');
  initialSocket.message(snapshot('old', 1));
  expect(manager.getStatus()).toBe('connected');
  expect(topology).toEqual(['old']);

  initialSocket.drop();
  scheduler.runNext();
  const recoverySocket = socketFactory.sockets[1] as FakeSocket;
  recoverySocket.open();

  expect(manager.getStatus()).toBe('resynced-with-possible-gap');
  expect(topology).toEqual(['old']);
  recoverySocket.message(snapshot('fresh', 2));
  expect(topology).toEqual(['old', 'fresh']);
  expect(manager.getStatus()).toBe('connected');
  expect(manager.getLastError()).toBeNull();
  expect(scheduler.pendingCount).toBe(0);
});

test('repeated recovery opens without a snapshot exhaust the bounded budget', () => {
  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  let topology = 'none';
  const manager = createAionClusterStreamManager({
    webSocketImpl: socketFactory.ctor,
    scheduler,
    reconnect: { initialDelayMs: 1, maxDelayMs: 1, maxAttempts: 2 },
    resyncTimeoutMs: 25,
    warn: () => undefined,
  });
  manager.subscribe({
    onSnapshot: (value) => {
      topology = value.node;
    },
    onEvent: () => undefined,
  });
  const initialSocket = socketFactory.sockets[0] as FakeSocket;
  initialSocket.open();
  initialSocket.message(snapshot('old', 1));
  initialSocket.drop();

  scheduler.runNext();
  const firstRecovery = socketFactory.sockets[1] as FakeSocket;
  firstRecovery.open();
  expect(manager.getStatus()).toBe('resynced-with-possible-gap');
  expect(topology).toBe('old');
  firstRecovery.drop();

  scheduler.runNext();
  const secondRecovery = socketFactory.sockets[2] as FakeSocket;
  secondRecovery.open();
  expect(manager.getStatus()).toBe('resynced-with-possible-gap');
  secondRecovery.drop();

  expect(socketFactory.sockets).toHaveLength(3);
  expect(manager.getStatus()).toBe('disconnected');
  expect(topology).toBe('old');
  expect(manager.getLastError()?.kind).toBe('reconnect-exhausted');
  expect(scheduler.pendingCount).toBe(0);
});

test('a missing priming snapshot times out and fails visibly at the recovery bound', () => {
  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  const errors: (AionSocketError | null)[] = [];
  const manager = createAionClusterStreamManager({
    webSocketImpl: socketFactory.ctor,
    scheduler,
    reconnect: { initialDelayMs: 1, maxAttempts: 1 },
    resyncTimeoutMs: 7,
    warn: () => undefined,
  });
  manager.onError((error) => errors.push(error));
  manager.subscribe({ onSnapshot: () => undefined, onEvent: () => undefined });
  const initialSocket = socketFactory.sockets[0] as FakeSocket;
  initialSocket.open();
  initialSocket.message(snapshot('old', 1));
  initialSocket.drop();
  scheduler.runNext();
  const recoverySocket = socketFactory.sockets[1] as FakeSocket;
  recoverySocket.open();

  expect(manager.getStatus()).toBe('resynced-with-possible-gap');
  expect(scheduler.pendingCount).toBe(1);
  scheduler.runNext();

  expect(recoverySocket.closeCalls).toBe(1);
  expect(manager.getStatus()).toBe('disconnected');
  expect(errors.map((error) => error?.kind)).toEqual(['subscriber-resync', 'reconnect-exhausted']);
  expect((errors[0]?.cause as Error).message).toContain('timed out after 7ms');
});

for (const [name, frame] of [
  ['malformed', '{not-json'],
  ['terminal', { error: { kind: 'ClusterLagged', skipped: 3 } }],
  [
    'pre-snapshot event',
    { kind: 'cluster_event', event: { type: 'PeerUp', meta: { cluster_seq: 2 } } },
  ],
] as const) {
  test(`${name} priming failure fail-stops instead of declaring connected`, () => {
    const scheduler = new FakeScheduler();
    const socketFactory = new FakeSocketFactory();
    const errors: (AionSocketError | null)[] = [];
    let events = 0;
    const manager = createAionClusterStreamManager({
      webSocketImpl: socketFactory.ctor,
      scheduler,
      reconnect: { maxAttempts: 0 },
      warn: () => undefined,
    });
    manager.onError((error) => errors.push(error));
    manager.subscribe({ onSnapshot: () => undefined, onEvent: () => (events += 1) });
    const socket = socketFactory.sockets[0] as FakeSocket;
    socket.open();
    socket.message(frame);

    expect(socket.closeCalls).toBe(1);
    expect(events).toBe(0);
    expect(errors[0]?.kind).toBe('frame-decode');
    expect(manager.getStatus()).toBe('disconnected');
    expect(manager.getLastError()?.kind).toBe('reconnect-exhausted');
  });
}

test('snapshot listener failure does not complete cluster recovery', () => {
  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  const errors: (AionSocketError | null)[] = [];
  const manager = createAionClusterStreamManager({
    webSocketImpl: socketFactory.ctor,
    scheduler,
    reconnect: { maxAttempts: 0 },
    warn: () => undefined,
  });
  manager.onError((error) => errors.push(error));
  manager.subscribe({
    onSnapshot: () => {
      throw new Error('topology reducer failed');
    },
    onEvent: () => undefined,
  });
  const socket = socketFactory.sockets[0] as FakeSocket;
  socket.open();
  socket.message(snapshot('never-applied', 1));

  expect(errors[0]?.kind).toBe('subscriber-application');
  expect(manager.getStatus()).toBe('disconnected');
  expect(socket.closeCalls).toBe(1);
});
