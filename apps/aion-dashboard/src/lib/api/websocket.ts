import type { Event, Namespace, WorkflowId, WorkflowStatus } from '@/types';

// AW WebSocket contract surface: update this one object when cluster AW pins the
// endpoint, subscribe/unsubscribe message keys, after-sequence cursor, or frame envelope shape.
const AW_WEBSOCKET_CONTRACT = {
  endpoint: '/events/stream',
  messageTypes: {
    subscribe: 'subscribe',
    unsubscribe: 'unsubscribe',
  },
  requestKeys: {
    type: 'type',
    subscriptionId: 'subscription_id',
    subscription: 'subscription',
    perWorkflow: 'per_workflow',
    filtered: 'filtered',
    firehose: 'firehose',
    namespace: 'namespace',
    namespaceSelector: 'namespace_selector',
    workflowId: 'workflow_id',
    workflowType: 'workflow_type',
    status: 'status',
    afterSequence: 'after_seq',
  },
  frameKeys: {
    namespace: 'namespace',
    event: 'event',
    payload: 'payload',
    payloadBytes: 'bytes',
  },
} as const;

type ReconnectOptions = {
  initialDelayMs: number;
  maxDelayMs: number;
  maxAttempts: number;
};

const DEFAULT_RECONNECT: ReconnectOptions = {
  initialDelayMs: 250,
  maxDelayMs: 5_000,
  maxAttempts: 5,
};

const SOCKET_OPEN = 1;
const SOCKET_CLOSING = 2;

export type ConnectionStatus = 'connected' | 'reconnecting' | 'disconnected';

export type WorkflowEventSubscriptionFilter = {
  kind: 'workflow';
  namespace: Namespace;
  workflowId: WorkflowId;
};

export type FilteredEventSubscriptionFilter = {
  kind: 'filtered';
  namespace: Namespace;
  workflowType?: string | null;
  status?: WorkflowStatus | null;
};

export type FirehoseEventSubscriptionFilter = {
  kind: 'firehose';
  namespace: Namespace;
};

export type AionEventSubscriptionFilter =
  | WorkflowEventSubscriptionFilter
  | FilteredEventSubscriptionFilter
  | FirehoseEventSubscriptionFilter;

export type AionEventHandler = (event: Event, context: AionEventContext) => void;

export type ResyncMode = 'after-sequence' | 'full-refetch';

export type AionEventContext = {
  subscriptionId: string;
  namespace: Namespace;
  filter: AionEventSubscriptionFilter;
};

export type ResyncContext = AionEventContext & {
  lastSeenSequence: number | null;
  mode: ResyncMode;
};

export type SubscribeOptions = {
  lastSeenSequence?: number;
  onResync?: (context: ResyncContext) => void;
};

export type AionEventWebSocketManagerOptions = {
  baseUrl?: string;
  webSocketImpl?: WebSocketConstructor;
  scheduler?: Scheduler;
  reconnect?: Partial<ReconnectOptions>;
};

type JsonRecord = Record<string, unknown>;
type Unsubscribe = () => void;
type StatusListener = (status: ConnectionStatus) => void;
type TransitionListener = () => void;
type WebSocketMessage = { data: unknown };
type WebSocketClose = { code?: number; reason?: string; wasClean: boolean };
type WebSocketEventHandler<T> = ((event: T) => void) | null;
type TimeoutHandle = ReturnType<typeof setTimeout>;

type ManagedWebSocket = {
  readonly readyState: number;
  onopen: WebSocketEventHandler<globalThis.Event>;
  onmessage: WebSocketEventHandler<WebSocketMessage>;
  onclose: WebSocketEventHandler<WebSocketClose>;
  onerror: WebSocketEventHandler<globalThis.Event>;
  send(data: string): void;
  close(): void;
};

type WebSocketConstructor = new (url: string) => ManagedWebSocket;

type Scheduler = {
  setTimeout(callback: () => void, delayMs: number): TimeoutHandle;
  clearTimeout(handle: TimeoutHandle): void;
};

type SubscriptionRecord = {
  id: string;
  filter: AionEventSubscriptionFilter;
  handler: AionEventHandler;
  lastSeenSequence: number | null;
  onResync?: (context: ResyncContext) => void;
};

export class AionEventWebSocketManager {
  private readonly baseUrl: string;
  private readonly webSocketImpl: WebSocketConstructor;
  private readonly scheduler: Scheduler;
  private readonly reconnect: ReconnectOptions;
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
  }

  connect(): void {
    if (this.socket !== null && this.socket.readyState < SOCKET_CLOSING) {
      return;
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
    this.socket = new this.webSocketImpl(buildWebSocketUrl(this.baseUrl));
    this.socket.onopen = () => {
      const recoveredFromDrop = this.status === 'reconnecting';
      this.reconnectAttempts = 0;
      this.setStatus('connected');
      this.notifyListeners(this.connectListeners);
      this.resendActiveSubscriptions();

      if (recoveredFromDrop) {
        this.notifyResyncHandlers();
      }
    };
    this.socket.onmessage = (message) => {
      this.handleMessage(message.data);
    };
    this.socket.onclose = () => {
      this.handleUnexpectedDisconnect();
    };
    this.socket.onerror = () => {
      this.handleUnexpectedDisconnect();
    };
  }

  private handleUnexpectedDisconnect(): void {
    if (this.intentionalClose) {
      return;
    }

    this.socket = null;
    this.setStatus('reconnecting');
    this.notifyListeners(this.disconnectListeners);
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
      console.warn('Unable to parse Aion event WebSocket frame', error);
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

function buildSubscribeMessage(subscription: SubscriptionRecord): JsonRecord {
  return {
    [AW_WEBSOCKET_CONTRACT.requestKeys.type]: AW_WEBSOCKET_CONTRACT.messageTypes.subscribe,
    [AW_WEBSOCKET_CONTRACT.requestKeys.subscriptionId]: subscription.id,
    [AW_WEBSOCKET_CONTRACT.requestKeys.subscription]: buildSubscriptionRequest(subscription),
  };
}

function buildUnsubscribeMessage(subscriptionId: string): JsonRecord {
  return {
    [AW_WEBSOCKET_CONTRACT.requestKeys.type]: AW_WEBSOCKET_CONTRACT.messageTypes.unsubscribe,
    [AW_WEBSOCKET_CONTRACT.requestKeys.subscriptionId]: subscriptionId,
  };
}

function buildSubscriptionRequest(subscription: SubscriptionRecord): JsonRecord {
  const afterSequence = subscription.lastSeenSequence;

  switch (subscription.filter.kind) {
    case 'workflow':
      return {
        [AW_WEBSOCKET_CONTRACT.requestKeys.perWorkflow]: withAfterSequence(
          {
            [AW_WEBSOCKET_CONTRACT.requestKeys.namespace]: subscription.filter.namespace,
            [AW_WEBSOCKET_CONTRACT.requestKeys.workflowId]: subscription.filter.workflowId,
          },
          afterSequence
        ),
      };
    case 'filtered':
      return {
        [AW_WEBSOCKET_CONTRACT.requestKeys.filtered]: withAfterSequence(
          {
            [AW_WEBSOCKET_CONTRACT.requestKeys.namespace]: subscription.filter.namespace,
            [AW_WEBSOCKET_CONTRACT.requestKeys.workflowType]: subscription.filter.workflowType ?? null,
            [AW_WEBSOCKET_CONTRACT.requestKeys.status]: subscription.filter.status ?? null,
          },
          afterSequence
        ),
      };
    case 'firehose':
      return {
        [AW_WEBSOCKET_CONTRACT.requestKeys.firehose]: withAfterSequence(
          {
            [AW_WEBSOCKET_CONTRACT.requestKeys.namespaceSelector]: subscription.filter.namespace,
          },
          afterSequence
        ),
      };
  }
}

function withAfterSequence(record: JsonRecord, sequence: number | null): JsonRecord {
  if (sequence === null) {
    return record;
  }

  return {
    ...record,
    [AW_WEBSOCKET_CONTRACT.requestKeys.afterSequence]: sequence,
  };
}

function buildResyncContext(subscription: SubscriptionRecord): ResyncContext {
  return {
    subscriptionId: subscription.id,
    namespace: subscription.filter.namespace,
    filter: subscription.filter,
    lastSeenSequence: subscription.lastSeenSequence,
    mode: subscription.lastSeenSequence === null ? 'full-refetch' : 'after-sequence',
  };
}

function parseFrame(data: unknown): { namespace: Namespace; event: Event } {
  const frame = parseJsonData(data);
  const event = readEventFromFrame(frame);
  const namespaceValue = isRecord(frame)
    ? frame[AW_WEBSOCKET_CONTRACT.frameKeys.namespace]
    : undefined;

  return {
    namespace: typeof namespaceValue === 'string' ? namespaceValue : event.data.envelope.workflow_id,
    event,
  };
}

function parseJsonData(data: unknown): unknown {
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

function readEventFromFrame(frame: unknown): Event {
  if (isEvent(frame)) {
    return frame;
  }

  if (!isRecord(frame)) {
    throw new Error('frame is not an object');
  }

  const eventValue = frame[AW_WEBSOCKET_CONTRACT.frameKeys.event];

  if (isEvent(eventValue)) {
    return eventValue;
  }

  const event = readEnvelopePayload(eventValue);

  if (isEvent(event)) {
    return event;
  }

  throw new Error('frame does not contain an Aion event');
}

function readEnvelopePayload(value: unknown): unknown {
  if (!isRecord(value)) {
    return value;
  }

  const payload = value[AW_WEBSOCKET_CONTRACT.frameKeys.payload];

  if (!isRecord(payload)) {
    return value;
  }

  const bytes = payload[AW_WEBSOCKET_CONTRACT.frameKeys.payloadBytes];

  if (Array.isArray(bytes)) {
    return JSON.parse(String.fromCharCode(...bytes)) as unknown;
  }

  if (typeof bytes === 'string') {
    return JSON.parse(decodeBase64(bytes)) as unknown;
  }

  return value;
}

function matchesSubscription(
  filter: AionEventSubscriptionFilter,
  namespace: Namespace,
  event: Event
): boolean {
  if (filter.namespace !== namespace) {
    return false;
  }

  switch (filter.kind) {
    case 'workflow':
      return event.data.envelope.workflow_id === filter.workflowId;
    case 'filtered':
      return filter.workflowType === undefined && filter.status === undefined
        ? true
        : matchesWorkflowType(filter.workflowType, event) && matchesWorkflowStatus(filter.status, event);
    case 'firehose':
      return true;
  }
}

function matchesWorkflowType(workflowType: string | null | undefined, event: Event): boolean {
  if (workflowType === undefined || workflowType === null) {
    return true;
  }

  return 'workflow_type' in event.data && event.data.workflow_type === workflowType;
}

function matchesWorkflowStatus(status: WorkflowStatus | null | undefined, event: Event): boolean {
  if (status === undefined || status === null) {
    return true;
  }

  return statusFromEvent(event) === status;
}

function statusFromEvent(event: Event): WorkflowStatus | null {
  switch (event.type) {
    case 'WorkflowStarted':
      return 'Running';
    case 'WorkflowCompleted':
      return 'Completed';
    case 'WorkflowFailed':
      return 'Failed';
    case 'WorkflowCancelled':
      return 'Cancelled';
    case 'WorkflowTimedOut':
      return 'TimedOut';
    default:
      return null;
  }
}

function isEvent(value: unknown): value is Event {
  if (!isRecord(value)) {
    return false;
  }

  const data = value.data;

  if (!isRecord(data)) {
    return false;
  }

  const envelope = data.envelope;

  return typeof value.type === 'string' && isRecord(envelope) && typeof envelope.seq === 'number';
}

function isRecord(value: unknown): value is JsonRecord {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function buildWebSocketUrl(baseUrl: string): string {
  const endpoint = AW_WEBSOCKET_CONTRACT.endpoint;

  if (baseUrl.length === 0 && typeof window !== 'undefined') {
    return `${window.location.origin.replace(/^http/, 'ws')}${endpoint}`;
  }

  if (baseUrl.startsWith('ws://') || baseUrl.startsWith('wss://')) {
    return `${baseUrl}${endpoint}`;
  }

  return `${baseUrl.replace(/^http/, 'ws')}${endpoint}`;
}

function stripTrailingSlash(value: string): string {
  return value.endsWith('/') ? value.slice(0, -1) : value;
}

function decodeBase64(value: string): string {
  if (typeof atob !== 'function') {
    throw new Error('base64 payload decoding requires atob');
  }

  return atob(value);
}

function browserWebSocketConstructor(): WebSocketConstructor {
  if (typeof WebSocket === 'undefined') {
    throw new Error('Aion event WebSocket manager requires a WebSocket implementation');
  }

  return WebSocket as unknown as WebSocketConstructor;
}
