import {
  closeWhenSafe,
  failFeedBoundary,
  isCurrentConnection,
  type SubscriptionConnection,
  notifyTransitionListeners,
  SubscriptionErrorState,
} from './websocket-connection';
import {
  assertExpectedWorkflowSequence,
  browserWebSocketConstructor,
  buildSubscribeMessage,
  buildWebSocketUrl,
  consoleWarn,
  frameDecodeError,
  parseFrame,
  reconnectExhaustedError,
  stripTrailingSlash,
  subscriberApplicationError,
} from './websocket-protocol';
import { ApplicationRecoveryPolicy } from './websocket-recovery-policy';
import {
  type AionEventHandler,
  type AionEventSubscriptionFilter,
  type AionEventWebSocketManagerOptions,
  type AionSocketError,
  type ConnectionStatus,
  DEFAULT_RECONNECT,
  DEFAULT_RESYNC_TIMEOUT_MS,
  type ManagedWebSocket,
  type ReconnectOptions,
  type Scheduler,
  SOCKET_CLOSING,
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
  ResyncHandler,
  ResyncMode,
  SocketCredentials,
  SubscribeOptions,
  WorkflowEventSubscriptionFilter,
} from './websocket-types';

/**
 * Owns one fail-stop socket per logical subscription.
 *
 * Durable feeds recover provably; live-only feeds degrade visibly. A decode or
 * subscriber failure immediately makes its socket stale, and durable cursors
 * advance only after successful application. Filtered and firehose reconnects
 * cannot prove continuity, so they expose a possible-gap state while delivering
 * new frames immediately. Only an awaited full refetch clears that marker.
 * Persistent decode, application, and timed-out/refused refetch failures consume
 * one shared bounded retry budget instead of crossing a cursor gap, buffering an
 * unbounded live tail, or reconnecting forever.
 */
export class AionEventWebSocketManager {
  private readonly baseUrl: string;
  private readonly credentials: SocketCredentials | undefined;
  private readonly webSocketImpl: WebSocketConstructor;
  private readonly scheduler: Scheduler;
  private readonly reconnect: ReconnectOptions;
  private readonly resyncTimeoutMs: number;
  private readonly warn: WarningLogger;
  private readonly connections = new Map<string, SubscriptionConnection>();
  private readonly statusListeners = new Set<StatusListener>();
  private readonly connectListeners = new Set<TransitionListener>();
  private readonly disconnectListeners = new Set<TransitionListener>();
  private readonly errorListeners = new Set<SocketErrorListener>();
  private readonly connectionErrors = new SubscriptionErrorState(
    this.connections,
    this.errorListeners
  );
  private readonly recoveryPolicy: ApplicationRecoveryPolicy;
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
    this.resyncTimeoutMs = options.resyncTimeoutMs ?? DEFAULT_RESYNC_TIMEOUT_MS;
    this.warn = options.warn ?? consoleWarn;
    this.recoveryPolicy = new ApplicationRecoveryPolicy(
      this.warn,
      this.connectionErrors,
      {
        isCurrent: (connection, socket) =>
          isCurrentConnection(this.connections, connection, socket),
        updateStatus: () => this.updateStatus(),
        failBoundary: (connection, socket) =>
          failFeedBoundary(socket, () => this.handleUnexpectedDisconnect(connection, socket)),
        exhaustBoundary: (connection, socket) => {
          connection.socket = null;
          closeWhenSafe(socket);
          this.updateStatus();
        },
      },
      this.scheduler,
      this.resyncTimeoutMs,
      this.reconnect.maxAttempts
    );
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
      this.recoveryPolicy.cancel(connection);
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
    const id = `aion-events-${this.nextSubscriptionId}`;
    this.nextSubscriptionId += 1;
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
      hasOpened: false,
      reconnectAttempts: 0,
      reconnectTimer: null,
      recoveryGeneration: 0,
      recoveryAttempt: null,
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
    this.recoveryPolicy.cancel(connection);
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
    if (this.intentionalClose || this.connections.get(connection.subscription.id) !== connection) {
      return;
    }

    this.clearReconnectTimer(connection);
    const recoveredFromDrop = connection.hasOpened;
    const socket = new this.webSocketImpl(buildWebSocketUrl(this.baseUrl, this.credentials));
    connection.socket = socket;
    socket.onopen = () => {
      if (!isCurrentConnection(this.connections, connection, socket)) {
        return;
      }
      connection.hasOpened = true;

      socket.send(JSON.stringify(buildSubscribeMessage(connection.subscription)));

      if (recoveredFromDrop && connection.subscription.filter.kind !== 'workflow') {
        // A live-only handshake cannot prove continuity. Deliver the new live tail
        // immediately, but keep the explicit gap marker and any boundary error
        // until a real full refetch succeeds.
        connection.state = 'possible-gap';
        this.updateStatus();
        this.recoveryPolicy.recoverLiveOnly(connection, socket);
        return;
      }

      if (recoveredFromDrop && this.recoveryPolicy.isUnresolved(connection.error)) {
        // The workflow socket is usable for replay, but is not healthy until one
        // event from the unchanged cursor is successfully applied.
        connection.state = 'reconnecting';
        this.updateStatus();
        this.recoveryPolicy.notifyDurable(connection, socket);
        return;
      }

      connection.reconnectAttempts = 0;
      connection.state = 'connected';
      this.connectionErrors.clear(connection);
      this.updateStatus();

      if (recoveredFromDrop) {
        this.recoveryPolicy.notifyDurable(connection, socket);
      }
    };
    socket.onmessage = (message) => {
      if (!isCurrentConnection(this.connections, connection, socket)) {
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
    if (this.intentionalClose || !isCurrentConnection(this.connections, connection, socket)) {
      return;
    }

    this.recoveryPolicy.cancel(connection);
    connection.socket = null;
    connection.state = 'reconnecting';
    const previousStatus = this.status;
    this.updateStatus();

    if (previousStatus !== 'reconnecting' && this.status === 'reconnecting') {
      notifyTransitionListeners(this.disconnectListeners);
    }

    this.scheduleReconnect(connection);
  }

  private scheduleReconnect(connection: SubscriptionConnection): void {
    if (connection.reconnectAttempts >= this.reconnect.maxAttempts) {
      connection.state = 'exhausted';
      this.connectionErrors.set(
        connection,
        reconnectExhaustedError(this.reconnect.maxAttempts, connection.subscription.id)
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

  private handleMessage(
    connection: SubscriptionConnection,
    socket: ManagedWebSocket,
    data: unknown
  ): void {
    let frame: ReturnType<typeof parseFrame>;

    try {
      frame = parseFrame(data);
      assertExpectedWorkflowSequence(connection.subscription, frame.event);
    } catch (error) {
      // No-silent-failure (M1): surface a typed error to listeners so the UI can
      // show that the feed dropped a frame. The console trail is secondary.
      this.warn('Unable to parse Aion event WebSocket frame', error);
      this.connectionErrors.set(connection, frameDecodeError(error, connection.subscription.id));
      failFeedBoundary(socket, () => this.handleUnexpectedDisconnect(connection, socket));
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
      this.connectionErrors.set(connection, subscriberApplicationError(error, subscription.id));

      failFeedBoundary(socket, () => this.handleUnexpectedDisconnect(connection, socket));
      return;
    }

    // The durable cursor means "already applied", not merely decoded/delivered.
    this.recoveryPolicy.markFrameDelivered(connection, socket);
    subscription.lastSeenSequence = frame.event.data.envelope.seq;
    if (
      this.recoveryPolicy.isUnresolved(connection.error) &&
      subscription.filter.kind === 'workflow'
    ) {
      this.recoveryPolicy.complete(connection, socket);
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
      } else if (connections.some((connection) => connection.state === 'possible-gap')) {
        nextStatus = 'resynced-with-possible-gap';
      } else if (connections.every((connection) => connection.state === 'connected')) {
        nextStatus = 'connected';
      }
    }

    const previousStatus = this.status;
    this.setStatus(nextStatus);

    if (previousStatus !== 'connected' && nextStatus === 'connected') {
      notifyTransitionListeners(this.connectListeners);
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
}

export function createAionEventWebSocketManager(
  options?: AionEventWebSocketManagerOptions
): AionEventWebSocketManager {
  return new AionEventWebSocketManager(options);
}

export const aionEventSocket = createAionEventWebSocketManager();
