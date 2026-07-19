import { expect, test } from 'bun:test';
import { createAionClusterStreamManager } from './cluster-stream';
import type { AionSocketError } from './websocket';
import { FakeScheduler, type FakeSocket, FakeSocketFactory } from './websocket-test-support';

function snapshot(node: string, asOfSeq: number) {
  return {
    kind: 'cluster_snapshot',
    snapshot: { node, as_of_seq: asOfSeq, peers: [], shards: [], workers: [] },
  };
}

function peerAdded(clusterSeq: number, peerName = 'peer-b') {
  return {
    kind: 'cluster_event',
    event: {
      type: 'PeerAdded',
      meta: {
        cluster_seq: clusterSeq,
        observed_at: '2026-07-19T12:00:00Z',
      },
      peer_name: peerName,
      forward_addr: null,
    },
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
  [
    'malformed snapshot',
    {
      kind: 'cluster_snapshot',
      snapshot: { node: 'broken', as_of_seq: -1, peers: [], shards: [], workers: [] },
    },
  ],
  [
    'malformed nested peer',
    {
      kind: 'cluster_snapshot',
      snapshot: {
        node: 'broken',
        as_of_seq: 1,
        peers: [{}],
        shards: [],
        workers: [],
      },
    },
  ],
  ['terminal', { error: { kind: 'ClusterLagged', skipped: 3 } }],
  ['pre-snapshot event', peerAdded(2)],
] as const) {
  test(`${name} priming failure fail-stops instead of declaring connected`, () => {
    const scheduler = new FakeScheduler();
    const socketFactory = new FakeSocketFactory();
    const errors: (AionSocketError | null)[] = [];
    let events = 0;
    let snapshots = 0;
    const manager = createAionClusterStreamManager({
      webSocketImpl: socketFactory.ctor,
      scheduler,
      reconnect: { maxAttempts: 0 },
      warn: () => undefined,
    });
    manager.onError((error) => errors.push(error));
    manager.subscribe({
      onSnapshot: () => {
        snapshots += 1;
      },
      onEvent: () => (events += 1),
    });
    const socket = socketFactory.sockets[0] as FakeSocket;
    socket.open();
    socket.message(frame);

    expect(socket.closeCalls).toBe(1);
    expect(snapshots).toBe(0);
    expect(events).toBe(0);
    expect(errors[0]?.kind).toBe('frame-decode');
    expect(manager.getStatus()).toBe('disconnected');
    expect(manager.getLastError()?.kind).toBe('reconnect-exhausted');
  });
}

for (const [name, frame] of [
  [
    'unknown',
    {
      kind: 'cluster_event',
      event: {
        type: 'PeerUp',
        meta: { cluster_seq: 2, observed_at: '2026-07-19T12:00:00Z' },
      },
    },
  ],
  [
    'missing-field',
    {
      kind: 'cluster_event',
      event: {
        type: 'PeerAdded',
        meta: { cluster_seq: 2, observed_at: '2026-07-19T12:00:00Z' },
        peer_name: 'peer-b',
      },
    },
  ],
] as const) {
  test(`${name} post-prime event fail-stops without advancing topology`, () => {
    const socketFactory = new FakeSocketFactory();
    const topology: string[] = [];
    let lastSeq = 0;
    const manager = createAionClusterStreamManager({
      webSocketImpl: socketFactory.ctor,
      reconnect: { maxAttempts: 0 },
      warn: () => undefined,
    });
    manager.subscribe({
      onSnapshot: (value) => {
        topology.push(value.node);
        lastSeq = value.as_of_seq;
      },
      onEvent: (event) => {
        topology.push(event.type);
        lastSeq = event.meta.cluster_seq;
      },
    });
    const socket = socketFactory.sockets[0] as FakeSocket;
    socket.open();
    socket.message(snapshot('healthy', 1));
    expect(manager.getStatus()).toBe('connected');

    socket.message(frame);

    expect(socket.closeCalls).toBe(1);
    expect(topology).toEqual(['healthy']);
    expect(lastSeq).toBe(1);
    expect(manager.getStatus()).toBe('disconnected');
    expect(manager.getLastError()?.kind).toBe('reconnect-exhausted');
  });
}

test('a restarted cluster publisher rebases to its fresh snapshot epoch', () => {
  const scheduler = new FakeScheduler();
  const socketFactory = new FakeSocketFactory();
  const peers: string[] = [];
  let provenance = -1;
  const manager = createAionClusterStreamManager({
    webSocketImpl: socketFactory.ctor,
    scheduler,
    reconnect: { initialDelayMs: 1, maxAttempts: 1 },
    warn: () => undefined,
  });
  manager.subscribe({
    onSnapshot: (value) => {
      provenance = value.as_of_seq;
    },
    onEvent: (event) => {
      peers.push(event.type === 'PeerAdded' ? event.peer_name : event.type);
      provenance = event.meta.cluster_seq;
    },
  });
  const initialSocket = socketFactory.sockets[0] as FakeSocket;
  initialSocket.open();
  expect(JSON.parse(initialSocket.sent[0] ?? '{}').subscription.cluster.after_seq).toBe(0);
  initialSocket.message(snapshot('old-process', 100));
  expect(provenance).toBe(100);

  initialSocket.drop();
  scheduler.runNext();
  const recoverySocket = socketFactory.sockets[1] as FakeSocket;
  recoverySocket.open();
  expect(JSON.parse(recoverySocket.sent[0] ?? '{}').subscription.cluster.after_seq).toBe(0);
  expect(manager.getStatus()).toBe('resynced-with-possible-gap');

  recoverySocket.message(snapshot('restarted-process', 0));
  expect(manager.getStatus()).toBe('connected');
  expect(provenance).toBe(0);
  recoverySocket.message(peerAdded(1, 'new-epoch-peer'));

  expect(peers).toEqual(['new-epoch-peer']);
  expect(provenance).toBe(1);
  expect(manager.getStatus()).toBe('connected');
});

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
