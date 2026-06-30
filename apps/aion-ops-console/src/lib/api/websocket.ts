import type { Event, Namespace } from '@/types';

import {
  browserWebSocketConstructor,
  buildResyncContext,
  buildSubscribeMessage,
  buildUnsubscribeMessage,
  buildWebSocketUrl,
  consoleWarn,
  frameDecodeError,
  matchesSubscription,
  parseFrame,
  reconnectExhaustedError,
  stripTrailingSlash,
} from './websocket-protocol';
import {
  type AionEventHandler,
  type AionEventSubscriptionFilter,
  type AionEventWebSocketManagerOptions,
  type AionSocketError,
  type ConnectionStatus,
  DEFAULT_RECONNECT,
  type ManagedWebSocket,
  type ReconnectOptions,
  type Scheduler,
  SOCKET_CLOSING,
  SOCKET_CONNECTING,
  SOCKET_OPEN,
  type SocketCredentials,
  type SocketErrorListener,
  type StatusListener,
  type SubscribeOptions,
  type SubscriptionRecord,
  type TimeoutHandle,
  type TransitionListener,
  type Unsubscribe,
  type WarningLogger,
  type WebSocketConstructor,
} from './websocket-types';

export type {
  AionEventContext,
  AionEventHandler,
  AionEventSubscriptionFilter,
  AionEventWebSocketManagerOptions,
  AionSocketError,
  AionSocketErrorKind,
  ConnectionStatus,
  FilteredEventSubscriptionFilter,
  FirehoseEventSubscriptionFilter,
  ResyncContext,
  ResyncMode,
  SocketCredentials,
  SubscribeOptions,
  WorkflowEventSubscriptionFilter,
} from './websocket-types';

export class AionEventWebSocketManager {
  private readonly baseUrl: string;
  private readonly credentials: SocketCredentials | undefined;
  private readonly webSocketImpl: WebSocketConstructor;
  private readonly scheduler: Scheduler;
  private readonly reconnect: ReconnectOptions;
  private readonly warn: WarningLogger;
  private readonly subscriptions = new Map<string, SubscriptionRecord>();
  private readonly statusListeners = new Set<StatusListener>();
  private readonly connectListeners = new Set<TransitionListener>();
  private readonly disconnectListeners = new Set<TransitionListener>();
  private readonly errorListeners = new Set<SocketErrorListener>();
  private socket: ManagedWebSocket | null = null;
  private status: ConnectionStatus = 'disconnected';
  private lastError: AionSocketError | null = null;
  private reconnectAttempts = 0;
  private reconnectTimer: TimeoutHandle | null = null;
  private intentionalClose = false;
  private nextSubscriptionId = 1;

  constructor(options: AionEventWebSocketManagerOptions = {}) {
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

  connect(): void {
    if (this.socket !== null && this.socket.readyState < SOCKET_CLOSING) {
      return;
    }

    if (this.status === 'disconnected') {
      this.reconnectAttempts = 0;
    }

    this.intentionalClose = false;
    this.openSocket();
  }

  close(): void {
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

  subscribe(
    filter: AionEventSubscriptionFilter,
    handler: AionEventHandler,
    options: SubscribeOptions = {}
  ): Unsubscribe {
    const id = this.allocateSubscriptionId();
    this.subscriptions.set(id, {
      id,
      filter,
      handler,
      lastSeenSequence: options.lastSeenSequence ?? null,
      onResync: options.onResync,
    });

    this.connect();
    this.sendSubscription(id);

    return () => {
      this.unsubscribe(id);
    };
  }

  unsubscribe(subscriptionId: string): void {
    if (!this.subscriptions.delete(subscriptionId)) {
      return;
    }

    this.sendJson(buildUnsubscribeMessage(subscriptionId));

    if (this.subscriptions.size === 0) {
      this.close();
    }
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

  onConnect(listener: TransitionListener): Unsubscribe {
    this.connectListeners.add(listener);

    return () => {
      this.connectListeners.delete(listener);
    };
  }

  onDisconnect(listener: TransitionListener): Unsubscribe {
    this.disconnectListeners.add(listener);

    return () => {
      this.disconnectListeners.delete(listener);
    };
  }

  /**
   * Subscribe to typed live-socket errors (M1: no-silent-failure). The manager
   * emits a non-null {@link AionSocketError} when a frame fails to decode or
   * reconnection is exhausted, and emits `null` once a healthy connection is
   * (re)established so a view can clear the error from visible state.
   */
  onError(listener: SocketErrorListener): Unsubscribe {
    this.errorListeners.add(listener);

    return () => {
      this.errorListeners.delete(listener);
    };
  }

  getLastError(): AionSocketError | null {
    return this.lastError;
  }

  updateLastSeenSequence(subscriptionId: string, sequence: number): void {
    const subscription = this.subscriptions.get(subscriptionId);

    if (subscription !== undefined) {
      subscription.lastSeenSequence = sequence;
    }
  }

  reset(): void {
    this.subscriptions.clear();
    this.statusListeners.clear();
    this.connectListeners.clear();
    this.disconnectListeners.clear();
    this.errorListeners.clear();
    this.lastError = null;
    this.close();
  }

  private openSocket(): void {
    this.clearReconnectTimer();
    const socket = new this.webSocketImpl(buildWebSocketUrl(this.baseUrl, this.credentials));
    this.socket = socket;
    socket.onopen = () => {
      if (this.socket !== socket) {
        return;
      }

      const recoveredFromDrop = this.status === 'reconnecting';
      this.reconnectAttempts = 0;
      this.clearError();
      this.setStatus('connected');
      this.notifyListeners(this.connectListeners);
      this.resendActiveSubscriptions();

      if (recoveredFromDrop) {
        this.notifyResyncHandlers();
      }
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

  private handleUnexpectedDisconnect(socket: ManagedWebSocket): void {
    if (this.intentionalClose || this.socket !== socket) {
      return;
    }

    this.socket = null;
    const wasAlreadyReconnecting = this.status === 'reconnecting';

    if (!wasAlreadyReconnecting) {
      this.setStatus('reconnecting');
      this.notifyListeners(this.disconnectListeners);
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

  private resendActiveSubscriptions(): void {
    for (const subscriptionId of this.subscriptions.keys()) {
      this.sendSubscription(subscriptionId);
    }
  }

  private notifyResyncHandlers(): void {
    for (const subscription of this.subscriptions.values()) {
      subscription.onResync?.(buildResyncContext(subscription));
    }
  }

  private sendSubscription(subscriptionId: string): void {
    const subscription = this.subscriptions.get(subscriptionId);

    if (subscription === undefined) {
      return;
    }

    this.sendJson(buildSubscribeMessage(subscription));
  }

  private sendJson(value: unknown): void {
    const socket = this.socket;

    if (socket === null || socket.readyState !== SOCKET_OPEN) {
      return;
    }

    socket.send(JSON.stringify(value));
  }

  private handleMessage(data: unknown): void {
    try {
      const frame = parseFrame(data);
      this.dispatch(frame.namespace, frame.event);
    } catch (error) {
      // No-silent-failure (M1): surface a typed error to listeners so the UI can
      // show that the feed dropped a frame. The console trail is secondary.
      this.warn('Unable to parse Aion event WebSocket frame', error);
      this.emitError(frameDecodeError(error));
    }
  }

  private dispatch(namespace: Namespace, event: Event): void {
    for (const subscription of this.subscriptions.values()) {
      if (matchesSubscription(subscription.filter, namespace, event)) {
        subscription.lastSeenSequence = event.data.envelope.seq;
        subscription.handler(event, {
          subscriptionId: subscription.id,
          namespace,
          filter: subscription.filter,
        });
      }
    }
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

  private notifyListeners(listeners: Set<TransitionListener>): void {
    for (const listener of listeners) {
      listener();
    }
  }

  private allocateSubscriptionId(): string {
    const id = `aion-events-${this.nextSubscriptionId}`;
    this.nextSubscriptionId += 1;
    return id;
  }
}

/**
 * Close a socket without ever calling `close()` while it is still CONNECTING.
 *
 * Browsers throw `DOMException: WebSocket is closed before the connection is
 * established` (logged as a console error) when `close()` is invoked on a socket
 * in the CONNECTING state — which happens under React StrictMode's double-mount,
 * where the cleanup runs before the just-opened socket has finished connecting.
 *
 * To tear down cleanly we defer the close to the `onopen` transition, then close
 * once the socket is OPEN. We overwrite the socket's listeners with no-op/closing
 * handlers so the abandoned socket can neither dispatch frames nor trigger the
 * manager's reconnect logic. If the connection never opens (it errors/closes
 * first), the deferred close is a harmless no-op on an already-closed socket.
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

export function createAionEventWebSocketManager(
  options?: AionEventWebSocketManagerOptions
): AionEventWebSocketManager {
  return new AionEventWebSocketManager(options);
}

export const aionEventSocket = createAionEventWebSocketManager();
