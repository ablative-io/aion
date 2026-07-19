import type { ClusterEvent, ClusterSnapshot } from '@/types';

import { clusterLaggedError, parseClusterFrame } from './cluster-stream-protocol';
import { closeWhenSafe } from './websocket-connection';
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
  DEFAULT_RESYNC_TIMEOUT_MS,
  type JsonRecord,
  type ManagedWebSocket,
  type ReconnectOptions,
  type Scheduler,
  SOCKET_CLOSING,
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
 * subscription multiplexed over the workflow socket. The dedicated socket sends
 * `{ subscription: { cluster: { after_seq } } }`, then must receive and apply a
 * priming `cluster_snapshot` before any live `cluster_event` is accepted.
 *
 * Transport open is never recovery proof. On reconnect the retained topology is
 * explicitly possibly gapped until a fresh snapshot is applied. Snapshot timeout,
 * malformed/terminal/pre-snapshot frames, and listener failures all fail-stop the
 * socket and consume one shared bounded reconnect budget.
 */

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
  /** Maximum wait for the fresh snapshot that proves a cluster socket current. */
  resyncTimeoutMs?: number;
  warn?: WarningLogger;
};

export type Unsubscribe = () => void;

/** Manages the single-subscriber dedicated cluster-stream socket. */
export class AionClusterStreamManager {
  private readonly baseUrl: string;
  private readonly credentials: SocketCredentials | undefined;
  private readonly webSocketImpl: WebSocketConstructor;
  private readonly scheduler: Scheduler;
  private readonly reconnect: ReconnectOptions;
  private readonly resyncTimeoutMs: number;
  private readonly warn: WarningLogger;
  private readonly listeners = new Set<ClusterStreamListener>();
  private readonly statusListeners = new Set<StatusListener>();
  private readonly errorListeners = new Set<SocketErrorListener>();
  private socket: ManagedWebSocket | null = null;
  private primingSocket: ManagedWebSocket | null = null;
  private status: ConnectionStatus = 'disconnected';
  private lastError: AionSocketError | null = null;
  private reconnectAttempts = 0;
  private reconnectTimer: TimeoutHandle | null = null;
  private primingTimer: TimeoutHandle | null = null;
  private intentionalClose = false;
  private hasAppliedSnapshot = false;
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
    this.resyncTimeoutMs = options.resyncTimeoutMs ?? DEFAULT_RESYNC_TIMEOUT_MS;
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
    return () => this.statusListeners.delete(listener);
  }

  onError(listener: SocketErrorListener): Unsubscribe {
    this.errorListeners.add(listener);
    return () => this.errorListeners.delete(listener);
  }

  getLastError(): AionSocketError | null {
    return this.lastError;
  }

  private connect(): void {
    if (this.socket !== null && this.socket.readyState < SOCKET_CLOSING) {
      return;
    }
    this.intentionalClose = false;
    this.setStatus('reconnecting');
    this.openSocket();
  }

  private close(): void {
    this.intentionalClose = true;
    this.clearReconnectTimer();
    this.clearPrimingTimer();
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

      this.sendSubscribe(socket);
      this.startPrimingTimeout(socket);
      this.setStatus(this.hasAppliedSnapshot ? 'resynced-with-possible-gap' : 'reconnecting');
    };
    socket.onmessage = (message) => {
      if (this.socket === socket) {
        this.handleMessage(socket, message.data);
      }
    };
    socket.onclose = () => this.handleUnexpectedDisconnect(socket);
    socket.onerror = () => {
      this.handleUnexpectedDisconnect(socket);
      closeWhenSafe(socket);
    };
  }

  private sendSubscribe(socket: ManagedWebSocket): void {
    if (socket.readyState !== SOCKET_OPEN) {
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

  private startPrimingTimeout(socket: ManagedWebSocket): void {
    this.clearPrimingTimer();
    this.primingSocket = socket;
    const timeout = this.scheduler.setTimeout(() => {
      if (
        this.socket !== socket ||
        this.primingSocket !== socket ||
        this.primingTimer !== timeout
      ) {
        return;
      }

      this.primingTimer = null;
      this.failSocket(
        socket,
        clusterPrimingError(new Error(`Cluster snapshot timed out after ${this.resyncTimeoutMs}ms`))
      );
    }, this.resyncTimeoutMs);
    this.primingTimer = timeout;
  }

  private handleMessage(socket: ManagedWebSocket, data: unknown): void {
    let frame: ReturnType<typeof parseClusterFrame>;
    try {
      frame = parseClusterFrame(data);
    } catch (error) {
      this.warn('Unable to parse Aion cluster-stream frame', error);
      this.failSocket(socket, frameDecodeError(error));
      return;
    }

    if (frame.kind === 'lagged') {
      this.lastAppliedSeq = 0;
      this.failSocket(socket, clusterLaggedError(frame.lagged));
      return;
    }
    if (frame.kind === 'snapshot') {
      if (this.primingSocket !== socket) {
        this.failSocket(socket, frameDecodeError(new Error('unexpected cluster snapshot')));
        return;
      }
      this.applySnapshot(socket, frame.snapshot);
      return;
    }
    if (this.primingSocket === socket) {
      this.failSocket(socket, frameDecodeError(new Error('cluster event arrived before snapshot')));
      return;
    }
    this.applyEvent(socket, frame.event);
  }

  private applySnapshot(socket: ManagedWebSocket, snapshot: ClusterSnapshot): void {
    try {
      for (const listener of this.listeners) {
        listener.onSnapshot(snapshot);
      }
    } catch (error) {
      this.failSocket(socket, clusterApplicationError(error));
      return;
    }
    if (this.socket !== socket || this.primingSocket !== socket) {
      return;
    }

    this.lastAppliedSeq = Math.max(this.lastAppliedSeq, snapshot.as_of_seq);
    this.hasAppliedSnapshot = true;
    this.clearPrimingTimer();
    this.reconnectAttempts = 0;
    this.clearError();
    this.setStatus('connected');
  }

  private applyEvent(socket: ManagedWebSocket, event: ClusterEvent): void {
    try {
      for (const listener of this.listeners) {
        listener.onEvent(event);
      }
    } catch (error) {
      this.failSocket(socket, clusterApplicationError(error));
      return;
    }
    if (this.socket === socket) {
      this.lastAppliedSeq = Math.max(this.lastAppliedSeq, event.meta.cluster_seq);
    }
  }

  private failSocket(socket: ManagedWebSocket, error: AionSocketError): void {
    if (this.socket !== socket) {
      return;
    }

    this.emitError(error);
    this.handleUnexpectedDisconnect(socket);
    closeWhenSafe(socket);
  }

  private handleUnexpectedDisconnect(socket: ManagedWebSocket): void {
    if (this.intentionalClose || this.socket !== socket) {
      return;
    }

    this.clearPrimingTimer();
    this.socket = null;
    this.setStatus('reconnecting');
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
      this.reconnectTimer = null;
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
    if (this.reconnectTimer !== null) {
      this.scheduler.clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
  }

  private clearPrimingTimer(): void {
    if (this.primingTimer !== null) {
      this.scheduler.clearTimeout(this.primingTimer);
      this.primingTimer = null;
    }
    this.primingSocket = null;
  }
}

function clusterPrimingError(cause: unknown): AionSocketError {
  return {
    kind: 'subscriber-resync',
    subscriptionId: null,
    message: 'The cluster stream did not produce a fresh snapshot; recovery will retry.',
    cause,
  };
}

function clusterApplicationError(cause: unknown): AionSocketError {
  return {
    kind: 'subscriber-application',
    subscriptionId: null,
    message: 'A fresh cluster snapshot or event could not be applied; recovery will retry.',
    cause,
  };
}

export function createAionClusterStreamManager(
  options?: ClusterStreamManagerOptions
): AionClusterStreamManager {
  return new AionClusterStreamManager(options);
}
