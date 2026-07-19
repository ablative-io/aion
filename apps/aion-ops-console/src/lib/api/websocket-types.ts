import type { Event, Namespace, WorkflowId, WorkflowStatus } from '@/types';

// Aion WebSocket contract surface. The server consumes one raw subscription as
// the first frame on each socket; it does not multiplex subscription ids.
export const AW_WEBSOCKET_CONTRACT = {
  endpoint: '/events/stream',
  requestKeys: {
    perWorkflow: 'per_workflow',
    filtered: 'filtered',
    firehose: 'firehose',
    namespace: 'namespace',
    namespaceSelector: 'namespace_selector',
    workflowId: 'workflow_id',
    workflowType: 'workflow_type',
    status: 'status',
    resumeFromSequence: 'resume_from_seq',
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

/** Maximum time a live-only full refetch may hold recovery open. */
export const DEFAULT_RESYNC_TIMEOUT_MS = 10_000;

export const SOCKET_CONNECTING = 0;
export const SOCKET_OPEN = 1;
export const SOCKET_CLOSING = 2;

export type ConnectionStatus =
  | 'connected'
  | 'resynced-with-possible-gap'
  | 'reconnecting'
  | 'disconnected';

/**
 * Kinds of live-socket failure surfaced to the UI. Each maps to a distinct,
 * human-readable cause so a view can render *why* the stream is degraded rather
 * than swallowing the error to the console.
 */
export type AionSocketErrorKind =
  | 'frame-decode'
  | 'subscriber-application'
  | 'subscriber-resync'
  | 'reconnect-exhausted';

/**
 * A typed live-socket error. M1 (no-silent-failure): instead of warning to the
 * console and dropping the failure, the manager emits this to error listeners so
 * the connection indicator and live views can show it as visible state.
 */
export type AionSocketError = {
  kind: AionSocketErrorKind;
  /** Logical event subscription that failed; null for dedicated stream managers. */
  subscriptionId: string | null;
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

/**
 * Reconnect recovery is durable only for per-workflow subscriptions. Live-only
 * filtered and firehose subscriptions always require a history refetch.
 */
export type ResyncMode = 'after-sequence' | 'full-refetch';

export type AionEventContext = {
  subscriptionId: string;
  namespace: Namespace;
  filter: AionEventSubscriptionFilter;
};

export type ResyncHandler = (context: ResyncContext) => void | Promise<void>;

/** One manager-owned recovery generation. Consumers must guard state commits with `isCurrent`. */
export type ResyncContext = AionEventContext & {
  lastSeenSequence: number | null;
  mode: ResyncMode;
  /** Monotonically increasing within this logical subscription. */
  generation: number;
  /** Aborted when this generation times out, disconnects, is superseded, or is cancelled. */
  signal: AbortSignal;
  /**
   * Transport abort is cooperative. Check this immediately before committing fetched state so
   * a response from an HTTP implementation that ignored `signal` cannot overwrite a newer recovery.
   */
  isCurrent: () => boolean;
};

export type SubscribeOptions = {
  /**
   * Highest per-workflow sequence already applied. Per-workflow reconnects ask
   * the server for the following sequence; filtered and firehose subscriptions
   * retain it only as refetch context and never put it on the wire.
   */
  lastSeenSequence?: number | undefined;
  /**
   * Best-effort recovery work performed after a reconnect. A live-only feed is
   * marked as possibly gapped until this callback fulfills. If omitted, that
   * honest degraded marker remains; rejection or timeout consumes the bounded
   * reconnect budget.
   */
  onResync?: ResyncHandler | undefined;
};

/**
 * Connection-level credentials for the live-events WebSocket. Browsers cannot
 * set request headers on a WebSocket handshake, so these are sent as query
 * parameters on the socket URL and the server promotes them back to their
 * header form (see `WsCaller` in aion-server). Every logical subscription gets
 * its own socket and reuses these manager-level credentials; its filter is
 * enforced by the server on that socket.
 */
export type SocketCredentials = {
  namespaces?: readonly Namespace[];
  subject?: string;
  bearerToken?: string;
  /**
   * Deployment-wide deploy grant. The cluster topology subscription is
   * deploy-scoped server-side (`cluster_stream.rs`), so its socket must carry
   * this grant or the server denies it with one terminal `namespace_denied`
   * frame and the stream reconnect-loops to "disconnected". In dev/no-auth mode
   * the grant rides as the `x-aion-deploy=true` query param (a browser cannot
   * set the header on a WS handshake); under real auth the grant lives in the
   * bearer token's `deploy` claim and the server ignores this param. Only the
   * cluster socket sets this — the workflow event socket is namespace-scoped and
   * must NOT carry it.
   */
  deploy?: boolean;
};

export type AionEventWebSocketManagerOptions = {
  baseUrl?: string;
  credentials?: SocketCredentials;
  webSocketImpl?: WebSocketConstructor;
  scheduler?: Scheduler;
  reconnect?: Partial<ReconnectOptions>;
  /** Timeout for an awaited live-only full refetch. Defaults to 10 seconds. */
  resyncTimeoutMs?: number;
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
  onResync?: ResyncHandler | undefined;
};
