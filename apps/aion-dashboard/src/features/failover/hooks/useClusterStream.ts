import { useEffect, useMemo, useRef, useState } from 'react';

import type { AionSocketError, ConnectionStatus } from '@/lib/api';
import type { AionClusterStreamManager, ClusterStreamListener } from '@/lib/api/cluster-stream';
import { configuredClusterStream } from '@/lib/config';
import type { ClusterEvent, ClusterPeer, ClusterShard, ClusterSnapshot } from '@/types';

/**
 * WS3 cluster-stream hook: the single source of LIVE cluster topology for the
 * failover view. Subscribes to the dedicated cluster socket, reduces the priming
 * {@link ClusterSnapshot} plus the live {@link ClusterEvent} deltas into a
 * coherent topology model, and exposes liveness / shards / workers / per-node
 * worker counts / provenance / typed error.
 *
 * Anti-polling (real-data rule): there is NO `setInterval`, NO `/health/live`
 * scrape, NO `/metrics` scrape. Every signal originates from the subsystem that
 * caused the change (supervisor tick, worker registry) and arrives as a push
 * frame. Liveness comes from `Peer*` deltas; worker counts from
 * `WorkerConnected`/`WorkerDisconnected` deltas reduced onto the snapshot.
 */

/** A peer's liveness as reduced from the snapshot + `Peer*` deltas. */
export type ClusterPeerState = ClusterPeer;

/** Reduced, render-ready cluster topology. */
export type ClusterTopology = {
  /** The reading node's own name (snapshot self-identity). */
  selfNode: string | null;
  /** Watched peers and their current liveness, keyed by name. */
  peers: ClusterPeerState[];
  /** Shards and their current owner/epoch. */
  shards: ClusterShard[];
  /** Connected workers (gated server-side to the caller's grant). */
  workers: ClusterSnapshot['workers'];
  /** Connected-worker count per node label (derived from worker deltas). */
  workersByNode: ReadonlyMap<string, number>;
};

export type ClusterStreamResult = {
  topology: ClusterTopology;
  /** True once a priming snapshot has been applied. */
  primed: boolean;
  /** Live-socket connection status. */
  status: ConnectionStatus;
  /** Highest `cluster_seq` applied (provenance — ADR-016). */
  lastSeq: number;
  /** UTC instant of the most recently applied frame, ISO-8601; null pre-prime. */
  lastObservedAt: string | null;
  /** Typed live-socket error, or null. No-silent-failure (ADR-016). */
  error: AionSocketError | null;
};

const EMPTY_TOPOLOGY: ClusterTopology = {
  selfNode: null,
  peers: [],
  shards: [],
  workers: [],
  workersByNode: new Map(),
};

/** Internal mutable reducer state (kept in a ref; React state mirrors it). */
type ReducerState = {
  selfNode: string | null;
  peers: Map<string, ClusterPeerState>;
  shards: Map<number, ClusterShard>;
  workers: Map<string, ClusterSnapshot['workers'][number]>;
  asOfSeq: number;
  lastSeq: number;
  lastObservedAt: string | null;
  primed: boolean;
};

function freshReducerState(): ReducerState {
  return {
    selfNode: null,
    peers: new Map(),
    shards: new Map(),
    workers: new Map(),
    asOfSeq: 0,
    lastSeq: 0,
    lastObservedAt: null,
    primed: false,
  };
}

function applySnapshot(state: ReducerState, snapshot: ClusterSnapshot): void {
  state.selfNode = snapshot.node;
  state.asOfSeq = snapshot.as_of_seq;
  state.lastSeq = Math.max(state.lastSeq, snapshot.as_of_seq);
  state.primed = true;

  state.peers = new Map(snapshot.peers.map((peer) => [peer.peer_name, peer]));
  state.shards = new Map(snapshot.shards.map((shard) => [shard.shard, shard]));
  state.workers = new Map(snapshot.workers.map((worker) => [worker.worker_id, worker]));

  // The snapshot frame carries no server instant yet, so stamp the prime with the
  // client receipt time — otherwise the provenance "last change" clock reads blank
  // until the first delta even though we just primed (Opus review M3). A server
  // snapshot timestamp is the cleaner long-term fix (ADR-027 keepalive work).
  state.lastObservedAt = new Date().toISOString();
}

/** Apply a single delta. Returns false when the delta is stale (already covered). */
function applyEvent(state: ReducerState, event: ClusterEvent): boolean {
  const seq = event.meta.cluster_seq;

  // `lastSeq` already subsumes `asOfSeq` (applySnapshot sets lastSeq >= asOfSeq),
  // so a single monotonic guard covers both the snapshot baseline and
  // already-applied deltas (Opus review M4: the previous nested form was dead).
  if (seq <= state.lastSeq) {
    return false;
  }

  state.lastSeq = Math.max(state.lastSeq, seq);
  state.lastObservedAt = event.meta.observed_at;

  switch (event.type) {
    case 'PeerAdded':
    case 'PeerConnected': {
      state.peers.set(event.peer_name, {
        peer_name: event.peer_name,
        forward_addr: event.forward_addr,
        connected: true,
        consecutive_down: 0,
      });
      break;
    }
    case 'PeerDisconnected': {
      const existing = state.peers.get(event.peer_name);
      state.peers.set(event.peer_name, {
        peer_name: event.peer_name,
        forward_addr: existing?.forward_addr ?? null,
        connected: false,
        consecutive_down: event.consecutive_down,
      });
      break;
    }
    case 'ShardAdopted': {
      const owner = event.adopted_by;
      for (const shard of event.shards) {
        // `ShardAdopted` does not carry the real haematite fence epoch — that
        // arrives via `ShardOwnerChanged`. Do NOT fabricate a monotonic counter
        // here (Opus review M1: an invented +1 looks real but isn't). Carry 0 as
        // an explicit placeholder and never surface it as the fence epoch.
        state.shards.set(shard, { shard, owner, epoch: 0 });
      }
      break;
    }
    case 'WorkerConnected': {
      state.workers.set(event.worker_id, {
        worker_id: event.worker_id,
        namespaces: event.namespaces,
        task_queue: event.task_queue,
        transport: event.transport,
        node: event.node,
      });
      break;
    }
    case 'WorkerDisconnected': {
      state.workers.delete(event.worker_id);
      break;
    }
    case 'SupervisorStarted':
    case 'SupervisorStopped': {
      state.selfNode = event.node;
      break;
    }
    // ShardAdoptionFailed / ShardAdoptionSkipped carry no topology mutation —
    // they are surfaced in the event log, not folded into the topology model.
    case 'ShardAdoptionFailed':
    case 'ShardAdoptionSkipped':
      break;
  }

  return true;
}

function projectTopology(state: ReducerState): ClusterTopology {
  const workers = [...state.workers.values()];
  const workersByNode = new Map<string, number>();
  for (const worker of workers) {
    if (worker.node !== null) {
      workersByNode.set(worker.node, (workersByNode.get(worker.node) ?? 0) + 1);
    }
  }

  return {
    selfNode: state.selfNode,
    peers: [...state.peers.values()],
    shards: [...state.shards.values()].sort((a, b) => a.shard - b.shard),
    workers,
    workersByNode,
  };
}

export type UseClusterStreamOptions = {
  /** Override the manager (tests); defaults to the configured singleton. */
  manager?: Pick<
    AionClusterStreamManager,
    'subscribe' | 'onStatusChange' | 'getStatus' | 'onError'
  >;
  /** Set false to not open the stream (e.g. an unselected namespace). */
  enabled?: boolean;
};

export function useClusterStream({
  manager = configuredClusterStream,
  enabled = true,
}: UseClusterStreamOptions = {}): ClusterStreamResult {
  const reducer = useRef<ReducerState>(freshReducerState());
  const [topology, setTopology] = useState<ClusterTopology>(EMPTY_TOPOLOGY);
  const [primed, setPrimed] = useState(false);
  const [lastSeq, setLastSeq] = useState(0);
  const [lastObservedAt, setLastObservedAt] = useState<string | null>(null);
  const [status, setStatus] = useState<ConnectionStatus>(() =>
    enabled ? manager.getStatus() : 'disconnected'
  );
  const [error, setError] = useState<AionSocketError | null>(null);

  useEffect(() => {
    if (!enabled) {
      return;
    }

    // Fresh reducer per (re)subscribe: a snapshot always re-bases the model.
    reducer.current = freshReducerState();

    const publish = (): void => {
      const state = reducer.current;
      setTopology(projectTopology(state));
      setPrimed(state.primed);
      setLastSeq(state.lastSeq);
      setLastObservedAt(state.lastObservedAt);
    };

    const listener: ClusterStreamListener = {
      onSnapshot: (snapshot) => {
        applySnapshot(reducer.current, snapshot);
        publish();
      },
      onEvent: (event) => {
        if (applyEvent(reducer.current, event)) {
          publish();
        }
      },
    };

    const unsubscribeStream = manager.subscribe(listener);
    const unsubscribeStatus = manager.onStatusChange(setStatus);
    const unsubscribeError = manager.onError(setError);
    setStatus(manager.getStatus());

    return () => {
      unsubscribeStream();
      unsubscribeStatus();
      unsubscribeError();
    };
  }, [manager, enabled]);

  return useMemo(
    () => ({ topology, primed, status, lastSeq, lastObservedAt, error }),
    [topology, primed, status, lastSeq, lastObservedAt, error]
  );
}
