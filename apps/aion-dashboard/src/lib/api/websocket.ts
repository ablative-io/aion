import type { Event, Namespace } from '@/types';

import {
  browserWebSocketConstructor,
  buildResyncContext,
  buildSubscribeMessage,
  buildUnsubscribeMessage,
  buildWebSocketUrl,
  consoleWarn,
  matchesSubscription,
  parseFrame,
  stripTrailingSlash,
} from './websocket-protocol';
import {
  type AionEventHandler,
  type AionEventSubscriptionFilter,
  type AionEventWebSocketManagerOptions,
  type ConnectionStatus,
  DEFAULT_RECONNECT,
  type ManagedWebSocket,
  type ReconnectOptions,
  type Scheduler,
  SOCKET_CLOSING,
  SOCKET_OPEN,
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
  ConnectionStatus,
  FilteredEventSubscriptionFilter,
  FirehoseEventSubscriptionFilter,
  ResyncContext,
  ResyncMode,
  SubscribeOptions,
  WorkflowEventSubscriptionFilter,
} from './websocket-types';

export class AionEventWebSocketManager {
  private readonly baseUrl: string;
  private readonly webSocketImpl: WebSocketConstructor;
  private readonly scheduler: Scheduler;
  private readonly reconnect: ReconnectOptions;
  private readonly warn: WarningLogger;
  private readonly subscriptions = new Map<string, SubscriptionRecord>();
  private readonly statusListeners = new Set<StatusListener>();
  private readonly connectListeners = new Set<TransitionListener>();
  private readonly disconnectListeners = new Set<TransitionListener>();
  private socket: ManagedWebSocket | null = null;
  private status: ConnectionStatus = 'disconnected';
  private reconnectAttempts = 0;
  private reconnectTimer: TimeoutHandle | null = null;
  private intentionalClose = false;
  private nextSubscriptionId = 1;

  constructor(options: AionEventWebSocketManagerOptions = {}) {
    this.baseUrl = stripTrailingSlash(options.baseUrl ?? '');
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
      currentSocket.close();
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
    this.close();
  }

  private openSocket(): void {
    this.clearReconnectTimer();
    const socket = new this.webSocketImpl(buildWebSocketUrl(this.baseUrl));
    this.socket = socket;
    socket.onopen = () => {
      if (this.socket !== socket) {
        return;
      }

      const recoveredFromDrop = this.status === 'reconnecting';
      this.reconnectAttempts = 0;
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
      this.warn('Unable to parse Aion event WebSocket frame', error);
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

export function createAionEventWebSocketManager(
  options?: AionEventWebSocketManagerOptions
): AionEventWebSocketManager {
  return new AionEventWebSocketManager(options);
}

export const aionEventSocket = createAionEventWebSocketManager();
