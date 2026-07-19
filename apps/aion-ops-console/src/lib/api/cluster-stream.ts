import type { ClusterEvent, ClusterSnapshot, ClusterStreamError } from '@/types';

import {
  browserWebSocketConstructor,
  buildWebSocketUrl,
  consoleWarn,
  frameDecodeError,
  reconnectExhaustedError,
  stripTrailingSlash,
} from './websocket-protocol';
import {
  type AionSocketError,
  type ConnectionStatus,
  DEFAULT_RECONNECT,
  type JsonRecord,
  type ManagedWebSocket,
  type ReconnectOptions,
  type Scheduler,
  SOCKET_CLOSING,
  SOCKET_CONNECTING,
  SOCKET_OPEN,
  type SocketCredentials,
  type SocketErrorListener,
  type StatusListener,
  type TimeoutHandle,
  type WarningLogger,
  type WebSocketConstructor,
} from './websocket-types';

/**
 * Cluster topology/ownership live-stream client (WS3).
 *
 * The server keeps `/events/stream` strictly one-subscription-per-socket: the
 * cluster channel is a NEW ARM of the single subscription frame, not a second
 * subscription multiplexed over the workflow socket. So the honest client is a
 * DEDICATED socket that opens `/events/stream`, sends exactly one
 * `{ subscription: { cluster: { after_seq } } }` frame, then receives a priming
 * `cluster_snapshot` followed by live `cluster_event` deltas (and, on overflow,
 * a terminal `cluster_lagged` error frame).
 *
 * This is the cluster analog of {@link AionEventWebSocketManager}: same reconnect
 * backoff, same typed {@link AionSocketError} no-silent-failure contract, same
 * query-param credential promotion (a browser cannot set WS handshake headers).
 * Reconnect re-requests the snapshot from the last applied `cluster_seq`
 * (`after_seq`), and a `cluster_lagged` terminal discards the cursor so the next
 * connect re-primes from a fresh snapshot (cluster history is non-durable).
 */

/** Server-frame discriminators pinned by `aion-proto` (`StreamedCluster*`). */
const CLUSTER_FRAME = {
  snapshot: 'cluster_snapshot',
  event: 'cluster_event',
} as const;

/** Wire keys on the client subscribe frame (mirrors `ws_subscription.rs`). */
const CLUSTER_REQUEST = {
  type: 'type',
  subscribe: 'subscribe',
  subscription: 'subscription',
  cluster: 'cluster',
  afterSeq: 'after_seq',
} as const;

export type ClusterStreamListener = {
  /** A fresh calm-state baseline; the reducer replaces its state from this. */
  onSnapshot: (snapshot: ClusterSnapshot) => void;
  /** A single live delta with `cluster_seq > snapshot.as_of_seq`. */
  onEvent: (event: ClusterEvent) => void;
};

export type ClusterStreamManagerOptions = {
  baseUrl?: string;
  credentials?: SocketCredentials;
  webSocketImpl?: WebSocketConstructor;
  scheduler?: Scheduler;
  reconnect?: Partial<ReconnectOptions>;
  warn?: WarningLogger;
};

export type Unsubscribe = () => void;

/**
 * Manages the dedicated cluster-stream socket. A single subscriber drives the
 * socket's lifetime: the first `subscribe()` opens it, the last `unsubscribe()`
 * closes it. (Phase 1 has exactly one consumer — the failover view — but the
 * Set keeps StrictMode's double-mount safe.)
 */
export class AionClusterStreamManager {
  private readonly baseUrl: string;
  private readonly credentials: SocketCredentials | undefined;
  private readonly webSocketImpl: WebSocketConstructor;
  private readonly scheduler: Scheduler;
  private readonly reconnect: ReconnectOptions;
  private readonly warn: WarningLogger;
  private readonly listeners = new Set<ClusterStreamListener>();
  private readonly statusListeners = new Set<StatusListener>();
  private readonly errorListeners = new Set<SocketErrorListener>();
  private socket: ManagedWebSocket | null = null;
  private status: ConnectionStatus = 'disconnected';
  private lastError: AionSocketError | null = null;
  private reconnectAttempts = 0;
  private reconnectTimer: TimeoutHandle | null = null;
  private intentionalClose = false;
  /** Highest `cluster_seq` applied; resumes the in-flight backlog on reconnect. */
  private lastAppliedSeq = 0;

  constructor(options: ClusterStreamManagerOptions = {}) {
    this.baseUrl = stripTrailingSlash(options.baseUrl ?? '');
    this.credentials = options.credentials;
    this.webSocketImpl = options.webSocketImpl ?? browserWebSocketConstructor();
    this.scheduler = options.scheduler ?? {
      setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
      clearTimeout: (handle) => clearTimeout(handle),
    };
    this.reconnect = { ...DEFAULT_RECONNECT, ...options.reconnect };
    this.warn = options.warn ?? consoleWarn;
  }

  subscribe(listener: ClusterStreamListener): Unsubscribe {
    this.listeners.add(listener);
    this.connect();

    return () => {
      this.listeners.delete(listener);
      if (this.listeners.size === 0) {
        this.close();
      }
    };
  }

  getStatus(): ConnectionStatus {
    return this.status;
  }

  onStatusChange(listener: StatusListener): Unsubscribe {
    this.statusListeners.add(listener);

    return () => {
      this.statusListeners.delete(listener);
    };
  }

  onError(listener: SocketErrorListener): Unsubscribe {
    this.errorListeners.add(listener);

    return () => {
      this.errorListeners.delete(listener);
    };
  }

  getLastError(): AionSocketError | null {
    return this.lastError;
  }

  private connect(): void {
    if (this.socket !== null && this.socket.readyState < SOCKET_CLOSING) {
      return;
    }

    if (this.status === 'disconnected') {
      this.reconnectAttempts = 0;
    }

    this.intentionalClose = false;
    this.openSocket();
  }

  private close(): void {
    this.intentionalClose = true;
    this.clearReconnectTimer();
    this.reconnectAttempts = 0;

    const currentSocket = this.socket;
    this.socket = null;

    if (currentSocket !== null) {
      closeWhenSafe(currentSocket);
    }

    this.setStatus('disconnected');
  }

  private openSocket(): void {
    this.clearReconnectTimer();
    const socket = new this.webSocketImpl(buildWebSocketUrl(this.baseUrl, this.credentials));
    this.socket = socket;

    socket.onopen = () => {
      if (this.socket !== socket) {
        return;
      }

      this.reconnectAttempts = 0;
      this.clearError();
      this.setStatus('connected');
      // Re-request from the last applied seq: the server suppresses buffered
      // deltas with `cluster_seq <= after_seq` and re-primes with a fresh
      // snapshot, so a brief drop resumes without re-applying seen deltas.
      this.sendSubscribe();
    };

    socket.onmessage = (message) => {
      if (this.socket !== socket) {
        return;
      }

      this.handleMessage(message.data);
    };

    socket.onclose = () => {
      this.handleUnexpectedDisconnect(socket);
    };

    socket.onerror = () => {
      this.handleUnexpectedDisconnect(socket);
      socket.close();
    };
  }

  private sendSubscribe(): void {
    const socket = this.socket;

    if (socket === null || socket.readyState !== SOCKET_OPEN) {
      return;
    }

    const frame: JsonRecord = {
      [CLUSTER_REQUEST.type]: CLUSTER_REQUEST.subscribe,
      [CLUSTER_REQUEST.subscription]: {
        [CLUSTER_REQUEST.cluster]: {
          [CLUSTER_REQUEST.afterSeq]: this.lastAppliedSeq,
        },
      },
    };

    socket.send(JSON.stringify(frame));
  }

  private handleMessage(data: unknown): void {
    let frame: unknown;

    try {
      frame = parseJson(data);
    } catch (error) {
      this.warn('Unable to parse Aion cluster-stream frame', error);
      this.emitError(frameDecodeError(error));
      return;
    }

    // Terminal `{ "error": { "kind": "ClusterLagged", "skipped": N } }`: the
    // client lost deltas. Surface it typed, then drop the resume cursor so the
    // next connect re-primes from a fresh snapshot (no durable history).
    const lagged = readClusterLagged(frame);
    if (lagged !== null) {
      this.lastAppliedSeq = 0;
      this.emitError(clusterLaggedError(lagged));
      return;
    }

    if (isSnapshotFrame(frame)) {
      this.lastAppliedSeq = Math.max(this.lastAppliedSeq, frame.snapshot.as_of_seq);
      this.clearError();
      for (const listener of this.listeners) {
        listener.onSnapshot(frame.snapshot);
      }
      return;
    }

    if (isEventFrame(frame)) {
      this.lastAppliedSeq = Math.max(this.lastAppliedSeq, frame.event.meta.cluster_seq);
      for (const listener of this.listeners) {
        listener.onEvent(frame.event);
      }
      return;
    }

    // An unrecognized frame is a contract drift, not a silent drop.
    this.warn('Unrecognized Aion cluster-stream frame', frame);
    this.emitError(frameDecodeError(new Error('unrecognized cluster-stream frame shape')));
  }

  private handleUnexpectedDisconnect(socket: ManagedWebSocket): void {
    if (this.intentionalClose || this.socket !== socket) {
      return;
    }

    this.socket = null;

    if (this.status !== 'reconnecting') {
      this.setStatus('reconnecting');
    }

    this.scheduleReconnect();
  }

  private scheduleReconnect(): void {
    if (this.reconnectAttempts >= this.reconnect.maxAttempts) {
      this.emitError(reconnectExhaustedError(this.reconnect.maxAttempts));
      this.setStatus('disconnected');
      return;
    }

    const delayMs = Math.min(
      this.reconnect.initialDelayMs * 2 ** this.reconnectAttempts,
      this.reconnect.maxDelayMs
    );
    this.reconnectAttempts += 1;
    this.reconnectTimer = this.scheduler.setTimeout(() => {
      this.openSocket();
    }, delayMs);
  }

  private emitError(error: AionSocketError): void {
    this.lastError = error;

    for (const listener of this.errorListeners) {
      listener(error);
    }
  }

  private clearError(): void {
    if (this.lastError === null) {
      return;
    }

    this.lastError = null;

    for (const listener of this.errorListeners) {
      listener(null);
    }
  }

  private setStatus(nextStatus: ConnectionStatus): void {
    if (this.status === nextStatus) {
      return;
    }

    this.status = nextStatus;

    for (const listener of this.statusListeners) {
      listener(nextStatus);
    }
  }

  private clearReconnectTimer(): void {
    if (this.reconnectTimer === null) {
      return;
    }

    this.scheduler.clearTimeout(this.reconnectTimer);
    this.reconnectTimer = null;
  }
}

type SnapshotFrame = { snapshot: ClusterSnapshot };
type EventFrame = { event: ClusterEvent };

function isRecord(value: unknown): value is JsonRecord {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function parseJson(data: unknown): unknown {
  if (typeof data === 'string') {
    return JSON.parse(data) as unknown;
  }

  if (data instanceof ArrayBuffer) {
    return JSON.parse(new TextDecoder().decode(data)) as unknown;
  }

  if (data instanceof Uint8Array) {
    return JSON.parse(new TextDecoder().decode(data)) as unknown;
  }

  return data;
}

function isSnapshotFrame(frame: unknown): frame is SnapshotFrame {
  if (!isRecord(frame) || frame.kind !== CLUSTER_FRAME.snapshot) {
    return false;
  }

  const snapshot = frame.snapshot;
  return (
    isRecord(snapshot) &&
    typeof snapshot.node === 'string' &&
    typeof snapshot.as_of_seq === 'number' &&
    Array.isArray(snapshot.peers) &&
    Array.isArray(snapshot.shards) &&
    Array.isArray(snapshot.workers)
  );
}

function isEventFrame(frame: unknown): frame is EventFrame {
  if (!isRecord(frame) || frame.kind !== CLUSTER_FRAME.event) {
    return false;
  }

  const event = frame.event;
  if (!isRecord(event) || typeof event.type !== 'string') {
    return false;
  }

  const meta = event.meta;
  return isRecord(meta) && typeof meta.cluster_seq === 'number';
}

function readClusterLagged(frame: unknown): ClusterStreamError | null {
  if (!isRecord(frame)) {
    return null;
  }

  const error = frame.error;
  if (!isRecord(error) || error.kind !== 'ClusterLagged' || typeof error.skipped !== 'number') {
    return null;
  }

  return { kind: 'ClusterLagged', skipped: error.skipped };
}

function clusterLaggedError(lagged: ClusterStreamError): AionSocketError {
  return {
    kind: 'frame-decode',
    subscriptionId: null,
    message: `The cluster stream fell behind and dropped ${lagged.skipped} update${
      lagged.skipped === 1 ? '' : 's'
    }; reconnecting to re-read the topology.`,
    cause: lagged,
  };
}

/**
 * Close a socket without ever calling `close()` while it is still CONNECTING
 * (mirrors {@link AionEventWebSocketManager}'s StrictMode-safe teardown).
 */
function closeWhenSafe(socket: ManagedWebSocket): void {
  if (socket.readyState === SOCKET_CONNECTING) {
    socket.onmessage = null;
    socket.onclose = null;
    socket.onerror = null;
    socket.onopen = () => {
      socket.close();
    };
    return;
  }

  socket.close();
}

export function createAionClusterStreamManager(
  options?: ClusterStreamManagerOptions
): AionClusterStreamManager {
  return new AionClusterStreamManager(options);
}
