import {
  browserWebSocketConstructor,
  buildResyncContext,
  buildSubscribeMessage,
  buildWebSocketUrl,
  consoleWarn,
  frameDecodeError,
  parseFrame,
  reconnectExhaustedError,
  stripTrailingSlash,
  subscriberApplicationError,
} from './websocket-protocol';
import {
  type SubscriptionConnection,
  SubscriptionErrorState,
} from './websocket-connection';
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
  type SocketCredentials,
  type SocketErrorListener,
  type StatusListener,
  type SubscribeOptions,
  type SubscriptionRecord,
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
  private readonly connections = new Map<string, SubscriptionConnection>();
  private readonly statusListeners = new Set<StatusListener>();
  private readonly connectListeners = new Set<TransitionListener>();
  private readonly disconnectListeners = new Set<TransitionListener>();
  private readonly errorListeners = new Set<SocketErrorListener>();
  private readonly connectionErrors = new SubscriptionErrorState(this.connections, this.errorListeners);
  private status: ConnectionStatus = 'disconnected';
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
    this.intentionalClose = false;

    // This explicit manager reactivation is the only operation that resets an
    // exhausted existing subscription. subscribe() activates only its new one.
    for (const connection of this.connections.values()) {
      if (
        (connection.socket !== null && connection.socket.readyState < SOCKET_CLOSING) ||
        connection.reconnectTimer !== null
      ) {
        continue;
      }

      connection.reconnectAttempts = 0;
      connection.state = 'idle';
      this.openSocket(connection);
    }

    this.updateStatus();
  }

  close(): void {
    this.intentionalClose = true;

    for (const connection of this.connections.values()) {
      this.clearReconnectTimer(connection);
      connection.reconnectAttempts = 0;
      connection.state = 'idle';
      const socket = connection.socket;
      connection.socket = null;

      if (socket !== null) {
        closeWhenSafe(socket);
      }
    }

    this.setStatus('disconnected');
  }

  subscribe(
    filter: AionEventSubscriptionFilter,
    handler: AionEventHandler,
    options: SubscribeOptions = {}
  ): Unsubscribe {
    const id = this.allocateSubscriptionId();
    const subscription: SubscriptionRecord = {
      id,
      filter,
      handler,
      lastSeenSequence: options.lastSeenSequence ?? null,
      onResync: options.onResync,
    };
    const connection: SubscriptionConnection = {
      subscription,
      socket: null,
      state: 'idle',
      reconnectAttempts: 0,
      reconnectTimer: null,
      error: null,
      errorSequence: 0,
    };
    this.connections.set(id, connection);

    // Do not use connect(): that is an explicit retry-all operation and would
    // silently resurrect unrelated subscriptions that exhausted their budget.
    this.intentionalClose = false;
    this.openSocket(connection);
    this.updateStatus();

    return () => {
      this.unsubscribe(id);
    };
  }

  unsubscribe(subscriptionId: string): void {
    const connection = this.connections.get(subscriptionId);

    if (connection === undefined) {
      return;
    }

    this.connections.delete(subscriptionId);
    this.clearReconnectTimer(connection);
    const socket = connection.socket;
    connection.socket = null;

    if (socket !== null) {
      closeWhenSafe(socket);
    }

    if (this.connections.size === 0) {
      this.intentionalClose = true;
    }
    this.connectionErrors.refresh();
    this.updateStatus();
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
   * emits a non-null {@link AionSocketError} when a frame fails to decode, a
   * subscriber fails to apply it, or reconnection is exhausted. Aggregate error
   * state stays non-null while any active logical subscription still has an
   * unresolved failure.
   */
  onError(listener: SocketErrorListener): Unsubscribe {
    this.errorListeners.add(listener);

    return () => {
      this.errorListeners.delete(listener);
    };
  }

  getLastError(): AionSocketError | null {
    return this.connectionErrors.getLastError();
  }

  updateLastSeenSequence(subscriptionId: string, sequence: number): void {
    const connection = this.connections.get(subscriptionId);

    if (connection !== undefined) {
      connection.subscription.lastSeenSequence = sequence;
    }
  }

  reset(): void {
    this.statusListeners.clear();
    this.connectListeners.clear();
    this.disconnectListeners.clear();
    this.errorListeners.clear();
    this.close();
    this.connections.clear();
    this.connectionErrors.reset();
  }

  private openSocket(connection: SubscriptionConnection): void {
    if (
      this.intentionalClose ||
      this.connections.get(connection.subscription.id) !== connection
    ) {
      return;
    }

    this.clearReconnectTimer(connection);
    const recoveredFromDrop = connection.state === 'reconnecting';
    const socket = new this.webSocketImpl(buildWebSocketUrl(this.baseUrl, this.credentials));
    connection.socket = socket;
    socket.onopen = () => {
      if (!this.isCurrentConnection(connection, socket)) {
        return;
      }

      connection.reconnectAttempts = 0;
      connection.state = 'connected';
      socket.send(JSON.stringify(buildSubscribeMessage(connection.subscription)));
      this.updateStatus();

      if (recoveredFromDrop) {
        connection.subscription.onResync?.(buildResyncContext(connection.subscription));
      }

      // A durable application failure is not resolved merely by reopening: its
      // unchanged cursor must replay and successfully apply the missed event.
      // Live-only streams resolve by initiating their required full refetch.
      if (
        connection.error?.kind !== 'subscriber-application' ||
        (recoveredFromDrop &&
          connection.subscription.filter.kind !== 'workflow' &&
          connection.subscription.onResync !== undefined)
      ) {
        this.connectionErrors.clear(connection);
      }
    };
    socket.onmessage = (message) => {
      if (!this.isCurrentConnection(connection, socket)) {
        return;
      }

      this.handleMessage(connection, socket, message.data);
    };
    socket.onclose = () => {
      this.handleUnexpectedDisconnect(connection, socket);
    };
    socket.onerror = () => {
      this.handleUnexpectedDisconnect(connection, socket);
      closeWhenSafe(socket);
    };
  }

  private handleUnexpectedDisconnect(
    connection: SubscriptionConnection,
    socket: ManagedWebSocket
  ): void {
    if (this.intentionalClose || !this.isCurrentConnection(connection, socket)) {
      return;
    }

    connection.socket = null;
    connection.state = 'reconnecting';
    const previousStatus = this.status;
    this.updateStatus();

    if (previousStatus !== 'reconnecting' && this.status === 'reconnecting') {
      this.notifyListeners(this.disconnectListeners);
    }

    this.scheduleReconnect(connection);
  }

  private scheduleReconnect(connection: SubscriptionConnection): void {
    if (connection.reconnectAttempts >= this.reconnect.maxAttempts) {
      connection.state = 'exhausted';
      this.connectionErrors.set(
        connection,
        reconnectExhaustedError(
          this.reconnect.maxAttempts,
          connection.subscription.id
        )
      );
      this.updateStatus();
      return;
    }

    const delayMs = Math.min(
      this.reconnect.initialDelayMs * 2 ** connection.reconnectAttempts,
      this.reconnect.maxDelayMs
    );
    connection.reconnectAttempts += 1;
    connection.reconnectTimer = this.scheduler.setTimeout(() => {
      connection.reconnectTimer = null;
      this.openSocket(connection);
    }, delayMs);
  }

  private isCurrentConnection(
    connection: SubscriptionConnection,
    socket: ManagedWebSocket
  ): boolean {
    return (
      this.connections.get(connection.subscription.id) === connection && connection.socket === socket
    );
  }

  private handleMessage(
    connection: SubscriptionConnection,
    socket: ManagedWebSocket,
    data: unknown
  ): void {
    let frame: ReturnType<typeof parseFrame>;

    try {
      frame = parseFrame(data);
    } catch (error) {
      // No-silent-failure (M1): surface a typed error to listeners so the UI can
      // show that the feed dropped a frame. The console trail is secondary.
      this.warn('Unable to parse Aion event WebSocket frame', error);
      this.connectionErrors.set(
        connection,
        frameDecodeError(error, connection.subscription.id)
      );
      return;
    }

    const subscription = connection.subscription;

    try {
      subscription.handler(frame.event, {
        subscriptionId: subscription.id,
        namespace: frame.namespace,
        filter: subscription.filter,
      });
    } catch (error) {
      this.warn('Aion event subscriber failed to apply a WebSocket frame', error);
      this.connectionErrors.set(
        connection,
        subscriberApplicationError(error, subscription.id)
      );

      // Stop this logical feed immediately so no later frame can overtake the
      // unapplied event. Per-workflow reconnect uses the unchanged cursor; live
      // subscriptions invoke onResync with full-refetch after reconnecting.
      this.handleUnexpectedDisconnect(connection, socket);
      closeWhenSafe(socket);
      return;
    }

    // The durable cursor means "already applied", not merely decoded/delivered.
    subscription.lastSeenSequence = frame.event.data.envelope.seq;
    if (
      connection.error?.kind === 'subscriber-application' &&
      subscription.filter.kind === 'workflow'
    ) {
      this.connectionErrors.clear(connection);
    }
  }

  private updateStatus(): void {
    let nextStatus: ConnectionStatus = 'disconnected';
    const connections = [...this.connections.values()];

    if (!this.intentionalClose && connections.length > 0) {
      if (connections.some((connection) => connection.state === 'exhausted')) {
        nextStatus = 'disconnected';
      } else if (connections.some((connection) => connection.state === 'reconnecting')) {
        nextStatus = 'reconnecting';
      } else if (connections.every((connection) => connection.state === 'connected')) {
        nextStatus = 'connected';
      }
    }

    const previousStatus = this.status;
    this.setStatus(nextStatus);

    if (previousStatus !== 'connected' && nextStatus === 'connected') {
      this.notifyListeners(this.connectListeners);
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

  private clearReconnectTimer(connection: SubscriptionConnection): void {
    if (connection.reconnectTimer === null) {
      return;
    }

    this.scheduler.clearTimeout(connection.reconnectTimer);
    connection.reconnectTimer = null;
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
