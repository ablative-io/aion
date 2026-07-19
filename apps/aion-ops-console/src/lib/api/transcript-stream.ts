import type { ActivityEvent, ActivityId, Namespace, WorkflowId } from '@/types';

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
 * Agent-transcript live-stream client (NOI-7).
 *
 * The server keeps `/events/stream` strictly one-subscription-per-socket, so the
 * transcript channel is a NEW ARM of the single subscription frame, NOT a second
 * subscription multiplexed over the workflow socket. The honest client is a
 * DEDICATED socket bound to ONE `(namespace, workflow, activity, attempt)` target:
 * it opens `/events/stream`, sends exactly one `{ subscription: { transcript: … } }`
 * frame, then receives the durable `activity_event` tail followed by live
 * `activity_event` deltas (and, on overflow, a terminal `transcript_lagged` error).
 *
 * REAL DATA ONLY, socket-first, NO polling: there is no `setInterval` anywhere.
 * Resume is by `store_seq` (the server-stamped commit sequence): the manager
 * tracks the highest applied `store_seq` and re-requests `after_seq` on reconnect,
 * so the server suppresses records `store_seq <= after_seq` and splices the live
 * tail with no gap and no duplicate. Ephemeral token deltas carry `store_seq: null`
 * and are forwarded live but never advance the resume cursor (they are never
 * replayed). A `transcript_lagged` terminal keeps the cursor: the durable `O` tail
 * is authoritative, so the next connect re-reads strictly after the last applied
 * `store_seq`.
 */

/** Server-frame discriminator pinned by `aion-proto` (`StreamedActivityEvent`). */
const TRANSCRIPT_FRAME = {
  event: 'activity_event',
} as const;

/** Wire keys on the client subscribe frame (mirrors `ws_subscription.rs`). */
const TRANSCRIPT_REQUEST = {
  type: 'type',
  subscribe: 'subscribe',
  subscription: 'subscription',
  transcript: 'transcript',
  namespace: 'namespace',
  workflowId: 'workflow_id',
  activityId: 'activity_id',
  attempt: 'attempt',
  afterSeq: 'after_seq',
} as const;

/** The `(namespace, workflow, activity, attempt)` target of one transcript socket. */
export type TranscriptTarget = {
  namespace: Namespace;
  workflowId: WorkflowId;
  activityId: ActivityId;
  attempt: number;
};

export type TranscriptStreamListener = {
  /** A single transcript event — durable replay OR live delta. */
  onEvent: (event: ActivityEvent) => void;
};

export type TranscriptStreamManagerOptions = {
  target: TranscriptTarget;
  baseUrl?: string;
  credentials?: SocketCredentials;
  webSocketImpl?: WebSocketConstructor;
  scheduler?: Scheduler;
  reconnect?: Partial<ReconnectOptions>;
  warn?: WarningLogger;
  /**
   * Resume cursor seeded from a REST backfill: the subscribe frame carries
   * `after_seq` immediately (on the FIRST connect), so the WS serves only the
   * live tail past the fetched history.
   */
  initialAfterSeq?: number;
};

export type Unsubscribe = () => void;

/**
 * Manages one dedicated transcript socket for a single attempt target. A single
 * subscriber drives the socket's lifetime: the first `subscribe()` opens it, the
 * last `unsubscribe()` closes it (the Set keeps StrictMode's double-mount safe).
 */
export class AionTranscriptStreamManager {
  private readonly target: TranscriptTarget;
  private readonly baseUrl: string;
  private readonly credentials: SocketCredentials | undefined;
  private readonly webSocketImpl: WebSocketConstructor;
  private readonly scheduler: Scheduler;
  private readonly reconnect: ReconnectOptions;
  private readonly warn: WarningLogger;
  private readonly listeners = new Set<TranscriptStreamListener>();
  private readonly statusListeners = new Set<StatusListener>();
  private readonly errorListeners = new Set<SocketErrorListener>();
  private socket: ManagedWebSocket | null = null;
  private status: ConnectionStatus = 'disconnected';
  private lastError: AionSocketError | null = null;
  private reconnectAttempts = 0;
  private reconnectTimer: TimeoutHandle | null = null;
  private intentionalClose = false;
  /**
   * Highest `store_seq` applied. `null` = nothing applied yet (a fresh
   * subscriber that must see the full durable transcript, including
   * `store_seq === 0`). A persisted event raises it; an ephemeral delta never
   * does (it carries `store_seq: null` and is never replayed).
   */
  private lastAppliedSeq: number | null = null;

  constructor(options: TranscriptStreamManagerOptions) {
    this.target = options.target;
    this.baseUrl = stripTrailingSlash(options.baseUrl ?? '');
    this.credentials = options.credentials;
    this.webSocketImpl = options.webSocketImpl ?? browserWebSocketConstructor();
    this.scheduler = options.scheduler ?? {
      setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
      clearTimeout: (handle) => clearTimeout(handle),
    };
    this.reconnect = { ...DEFAULT_RECONNECT, ...options.reconnect };
    this.warn = options.warn ?? consoleWarn;
    this.lastAppliedSeq = options.initialAfterSeq ?? null;
  }

  subscribe(listener: TranscriptStreamListener): Unsubscribe {
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
      // Re-request from the last applied store_seq: the server suppresses
      // durable records + live deltas with store_seq <= after_seq and splices
      // the live tail, so a brief drop resumes with no gap and no duplicate.
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

    const transcript: JsonRecord = {
      [TRANSCRIPT_REQUEST.namespace]: this.target.namespace,
      [TRANSCRIPT_REQUEST.workflowId]: this.target.workflowId,
      [TRANSCRIPT_REQUEST.activityId]: this.target.activityId,
      [TRANSCRIPT_REQUEST.attempt]: this.target.attempt,
    };
    // Only send after_seq once at least one durable event has been applied; a
    // fresh subscriber omits it and receives the full durable transcript.
    if (this.lastAppliedSeq !== null) {
      transcript[TRANSCRIPT_REQUEST.afterSeq] = this.lastAppliedSeq;
    }

    const frame: JsonRecord = {
      [TRANSCRIPT_REQUEST.type]: TRANSCRIPT_REQUEST.subscribe,
      [TRANSCRIPT_REQUEST.subscription]: {
        [TRANSCRIPT_REQUEST.transcript]: transcript,
      },
    };

    socket.send(JSON.stringify(frame));
  }

  private handleMessage(data: unknown): void {
    let frame: unknown;

    try {
      frame = parseJson(data);
    } catch (error) {
      this.warn('Unable to parse Aion transcript-stream frame', error);
      this.emitError(frameDecodeError(error));
      return;
    }

    // Terminal `{ "error": { "code": "transcript_lagged", "skipped": N } }`: the
    // client lost live deltas. Surface it typed but KEEP the resume cursor — the
    // durable O tail is authoritative, so the next connect re-reads strictly
    // after the last applied store_seq (no gap, no duplicate).
    const lagged = readTranscriptLagged(frame);
    if (lagged !== null) {
      this.emitError(transcriptLaggedError(lagged));
      return;
    }

    const event = readActivityEventFrame(frame);
    if (event !== null) {
      // A persisted event advances the resume cursor; an ephemeral delta
      // (store_seq: null) is forwarded live but never advances it.
      if (typeof event.store_seq === 'number') {
        this.lastAppliedSeq = Math.max(this.lastAppliedSeq ?? 0, event.store_seq);
      }
      for (const listener of this.listeners) {
        listener.onEvent(event);
      }
      return;
    }

    // An unrecognized frame is a contract drift, not a silent drop.
    this.warn('Unrecognized Aion transcript-stream frame', frame);
    this.emitError(frameDecodeError(new Error('unrecognized transcript-stream frame shape')));
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

/** A `transcript_lagged` terminal — the count of live deltas the client missed. */
export type TranscriptLagged = { skipped: number };

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

/**
 * Narrow a server frame to the wrapped {@link ActivityEvent}, or `null` when the
 * frame is not an `activity_event`. The inner event is the ts-rs-generated shape;
 * we validate only the discriminator + the load-bearing fields the resume cursor
 * and rendering depend on, so a malformed frame is surfaced (not silently kept).
 */
function readActivityEventFrame(frame: unknown): ActivityEvent | null {
  if (!isRecord(frame) || frame.kind !== TRANSCRIPT_FRAME.event) {
    return null;
  }

  const event = frame.event;
  if (!isRecord(event)) {
    return null;
  }

  const eventKind = event.kind;
  const storeSeq = event.store_seq;
  const validSeq = storeSeq === null || typeof storeSeq === 'number';
  if (!isRecord(eventKind) || typeof eventKind.kind !== 'string' || !validSeq) {
    return null;
  }

  return event as unknown as ActivityEvent;
}

function readTranscriptLagged(frame: unknown): TranscriptLagged | null {
  if (!isRecord(frame)) {
    return null;
  }

  const error = frame.error;
  if (!isRecord(error) || error.code !== 'transcript_lagged' || typeof error.skipped !== 'number') {
    return null;
  }

  return { skipped: error.skipped };
}

function transcriptLaggedError(lagged: TranscriptLagged): AionSocketError {
  return {
    kind: 'frame-decode',
    subscriptionId: null,
    message: `The transcript stream fell behind and dropped ${lagged.skipped} live update${
      lagged.skipped === 1 ? '' : 's'
    }; reconnecting to re-read the durable tail.`,
    cause: lagged,
  };
}

/**
 * Close a socket without ever calling `close()` while it is still CONNECTING
 * (mirrors {@link AionClusterStreamManager}'s StrictMode-safe teardown).
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

export function createAionTranscriptStreamManager(
  options: TranscriptStreamManagerOptions
): AionTranscriptStreamManager {
  return new AionTranscriptStreamManager(options);
}
