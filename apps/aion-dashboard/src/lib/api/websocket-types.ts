import type { Event, Namespace, WorkflowId, WorkflowStatus } from '@/types';

// AW WebSocket contract surface: update this one object when cluster AW pins the
// endpoint, subscribe/unsubscribe message keys, after-sequence cursor, or frame envelope shape.
export const AW_WEBSOCKET_CONTRACT = {
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

export type ReconnectOptions = {
  initialDelayMs: number;
  maxDelayMs: number;
  maxAttempts: number;
};

export const DEFAULT_RECONNECT: ReconnectOptions = {
  initialDelayMs: 250,
  maxDelayMs: 5_000,
  maxAttempts: 5,
};

export const SOCKET_OPEN = 1;
export const SOCKET_CLOSING = 2;

export type ConnectionStatus = 'connected' | 'reconnecting' | 'disconnected';

/**
 * Kinds of live-socket failure surfaced to the UI. Each maps to a distinct,
 * human-readable cause so a view can render *why* the stream is degraded rather
 * than swallowing the error to the console.
 */
export type AionSocketErrorKind = 'frame-decode' | 'reconnect-exhausted';

/**
 * A typed live-socket error. M1 (no-silent-failure): instead of warning to the
 * console and dropping the failure, the manager emits this to error listeners so
 * the connection indicator and live views can show it as visible state.
 */
export type AionSocketError = {
  kind: AionSocketErrorKind;
  /** Operator-facing message; safe to render directly. */
  message: string;
  /** The underlying error/value, kept for the console trail. */
  cause: unknown;
};

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
  warn?: WarningLogger;
};

export type JsonRecord = Record<string, unknown>;
export type ConsoleLike = { warn(message: string, error: unknown): void };
export type Unsubscribe = () => void;
export type StatusListener = (status: ConnectionStatus) => void;
export type SocketErrorListener = (error: AionSocketError | null) => void;
export type TransitionListener = () => void;
export type WarningLogger = (message: string, error: unknown) => void;
export type WebSocketMessage = { data: unknown };
export type WebSocketClose = { code?: number; reason?: string; wasClean: boolean };
export type WebSocketEventHandler<T> = ((event: T) => void) | null;
export type TimeoutHandle = ReturnType<typeof setTimeout>;

export type ManagedWebSocket = {
  readonly readyState: number;
  onopen: WebSocketEventHandler<globalThis.Event>;
  onmessage: WebSocketEventHandler<WebSocketMessage>;
  onclose: WebSocketEventHandler<WebSocketClose>;
  onerror: WebSocketEventHandler<globalThis.Event>;
  send(data: string): void;
  close(): void;
};

export type WebSocketConstructor = new (url: string) => ManagedWebSocket;

export type Scheduler = {
  setTimeout(callback: () => void, delayMs: number): TimeoutHandle;
  clearTimeout(handle: TimeoutHandle): void;
};

export type SubscriptionRecord = {
  id: string;
  filter: AionEventSubscriptionFilter;
  handler: AionEventHandler;
  lastSeenSequence: number | null;
  onResync?: (context: ResyncContext) => void;
};
