import type { Event, Namespace } from '@/types';

import {
  type AionSocketError,
  AW_WEBSOCKET_CONTRACT,
  type ConsoleLike,
  type JsonRecord,
  type ManagedWebSocket,
  type ResyncContext,
  type SocketCredentials,
  type SubscriptionRecord,
  type WarningLogger,
  type WebSocketClose,
  type WebSocketConstructor,
  type WebSocketEventHandler,
  type WebSocketMessage,
} from './websocket-types';

export const globalConsole = globalThis.console as ConsoleLike;

export const consoleWarn: WarningLogger = (message, error) => {
  globalConsole.warn(message, error);
};

export function buildSubscribeMessage(subscription: SubscriptionRecord): JsonRecord {
  return buildSubscriptionRequest(subscription);
}

function buildSubscriptionRequest(subscription: SubscriptionRecord): JsonRecord {
  switch (subscription.filter.kind) {
    case 'workflow':
      return {
        [AW_WEBSOCKET_CONTRACT.requestKeys.perWorkflow]: withResumeFromSequence(
          {
            [AW_WEBSOCKET_CONTRACT.requestKeys.namespace]: subscription.filter.namespace,
            [AW_WEBSOCKET_CONTRACT.requestKeys.workflowId]: subscription.filter.workflowId,
          },
          subscription.lastSeenSequence
        ),
      };
    case 'filtered':
      return {
        [AW_WEBSOCKET_CONTRACT.requestKeys.filtered]: {
          [AW_WEBSOCKET_CONTRACT.requestKeys.namespace]: subscription.filter.namespace,
          [AW_WEBSOCKET_CONTRACT.requestKeys.workflowType]:
            subscription.filter.workflowType ?? null,
          [AW_WEBSOCKET_CONTRACT.requestKeys.status]: subscription.filter.status ?? null,
        },
      };
    case 'firehose':
      return {
        [AW_WEBSOCKET_CONTRACT.requestKeys.firehose]: {
          [AW_WEBSOCKET_CONTRACT.requestKeys.namespaceSelector]: subscription.filter.namespace,
        },
      };
  }
}

function withResumeFromSequence(record: JsonRecord, lastSeenSequence: number | null): JsonRecord {
  if (lastSeenSequence === null) {
    return record;
  }

  return {
    ...record,
    [AW_WEBSOCKET_CONTRACT.requestKeys.resumeFromSequence]: lastSeenSequence + 1,
  };
}

export function frameDecodeError(
  cause: unknown,
  subscriptionId: string | null = null
): AionSocketError {
  return {
    kind: 'frame-decode',
    subscriptionId,
    message: 'A live event could not be decoded; the feed may be missing entries.',
    cause,
  };
}

export function subscriberApplicationError(
  cause: unknown,
  subscriptionId: string
): AionSocketError {
  return {
    kind: 'subscriber-application',
    subscriptionId,
    message: 'A live event could not be applied; reconnecting without advancing its cursor.',
    cause,
  };
}

export function subscriberResyncError(cause: unknown, subscriptionId: string): AionSocketError {
  return {
    kind: 'subscriber-application',
    subscriptionId,
    message: 'Live state could not be resynchronized; recovery will retry within its limit.',
    cause,
  };
}

export function reconnectExhaustedError(
  attempts: number,
  subscriptionId: string | null = null
): AionSocketError {
  return {
    kind: 'reconnect-exhausted',
    subscriptionId,
    message: `Live connection lost after ${attempts} reconnect attempt${
      attempts === 1 ? '' : 's'
    }. Reload to retry.`,
    cause: null,
  };
}

export function buildResyncContext(subscription: SubscriptionRecord): ResyncContext {
  return {
    subscriptionId: subscription.id,
    namespace: subscription.filter.namespace,
    filter: subscription.filter,
    lastSeenSequence: subscription.lastSeenSequence,
    mode:
      subscription.filter.kind === 'workflow' && subscription.lastSeenSequence !== null
        ? 'after-sequence'
        : 'full-refetch',
  };
}

export function parseFrame(data: unknown): { namespace: Namespace; event: Event } {
  const frame = parseJsonData(data);
  const event = readEventFromFrame(frame);
  const namespaceValue = isRecord(frame)
    ? frame[AW_WEBSOCKET_CONTRACT.frameKeys.namespace]
    : undefined;

  if (typeof namespaceValue !== 'string') {
    throw new Error('frame does not contain a namespace');
  }

  return {
    namespace: namespaceValue,
    event,
  };
}

export function assertExpectedWorkflowSequence(
  subscription: SubscriptionRecord,
  event: Event
): void {
  const previous = subscription.lastSeenSequence;

  if (subscription.filter.kind !== 'workflow' || previous === null) {
    return;
  }

  const received = event.data.envelope.seq;
  if (received !== previous + 1) {
    throw new Error(`workflow replay sequence gap: expected ${previous + 1}, received ${received}`);
  }
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
    return JSON.parse(new TextDecoder().decode(Uint8Array.from(bytes))) as unknown;
  }

  if (typeof bytes === 'string') {
    return JSON.parse(new TextDecoder().decode(decodeBase64Bytes(bytes))) as unknown;
  }

  return value;
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

export function buildWebSocketUrl(baseUrl: string, credentials?: SocketCredentials): string {
  const base = resolveSocketBase(baseUrl, AW_WEBSOCKET_CONTRACT.endpoint);
  const query = buildCredentialQuery(credentials);

  return query.length === 0 ? base : `${base}?${query}`;
}

function resolveSocketBase(baseUrl: string, endpoint: string): string {
  if (baseUrl.length === 0 && typeof window !== 'undefined') {
    return `${window.location.origin.replace(/^http/, 'ws')}${endpoint}`;
  }

  if (baseUrl.startsWith('ws://') || baseUrl.startsWith('wss://')) {
    return `${baseUrl}${endpoint}`;
  }

  return `${baseUrl.replace(/^http/, 'ws')}${endpoint}`;
}

/**
 * Encode connection-level credentials as query parameters. Browsers cannot set
 * request headers on a WebSocket handshake, so the server accepts these on the
 * URL and promotes them back to header form (`WsCaller`). The bearer token is
 * sent as `access_token`, which the server wraps in the `Bearer` scheme.
 */
function buildCredentialQuery(credentials?: SocketCredentials): string {
  if (credentials === undefined) {
    return '';
  }

  const params = new URLSearchParams();

  if (credentials.namespaces !== undefined && credentials.namespaces.length > 0) {
    params.set('x-aion-namespaces', credentials.namespaces.join(','));
  }

  if (credentials.subject !== undefined && credentials.subject.length > 0) {
    params.set('x-aion-subject', credentials.subject);
  }

  if (credentials.bearerToken !== undefined && credentials.bearerToken.length > 0) {
    params.set('access_token', credentials.bearerToken);
  }

  // Deploy-scoped cluster sockets carry the deployment-wide grant. In dev/no-auth
  // mode the server promotes `x-aion-deploy=true` to the dev deploy grant; under
  // real auth it ignores this param and reads the bearer token's `deploy` claim,
  // so sending it is harmless there. Only set when explicitly granted.
  if (credentials.deploy === true) {
    params.set('x-aion-deploy', 'true');
  }

  return params.toString();
}

export function stripTrailingSlash(value: string): string {
  return value.endsWith('/') ? value.slice(0, -1) : value;
}

function decodeBase64Bytes(value: string): Uint8Array {
  const binary = atob(value);
  return Uint8Array.from(binary, (char) => char.charCodeAt(0));
}

export function browserWebSocketConstructor(): WebSocketConstructor {
  if (typeof WebSocket === 'undefined') {
    throw new Error('Aion event WebSocket manager requires a WebSocket implementation');
  }

  // The DOM `WebSocket` is structurally wider than {@link ManagedWebSocket}: its
  // handler slots are typed with the full DOM event types, which makes the raw
  // constructor non-assignable to `WebSocketConstructor` under strict function
  // variance. Rather than cast through `unknown`, we adapt the instance — the
  // wrapper exposes exactly the narrowed surface the manager uses, forwarding the
  // DOM events (which structurally satisfy the narrowed event shapes) to the
  // manager's handlers and delegating `send`/`close`/`readyState` to the socket.
  return class implements ManagedWebSocket {
    private readonly socket: WebSocket;
    // The manager only ever *assigns* handlers; it never reads them back. We keep
    // each assigned handler in our own narrowed-typed field (the source of truth
    // for the getter) and forward the DOM event — which structurally satisfies the
    // narrowed shape — into it. This makes the getter return our exact type while
    // the DOM socket still drives the callback.
    #onopen: WebSocketEventHandler<globalThis.Event> = null;
    #onmessage: WebSocketEventHandler<WebSocketMessage> = null;
    #onclose: WebSocketEventHandler<WebSocketClose> = null;
    #onerror: WebSocketEventHandler<globalThis.Event> = null;

    constructor(url: string) {
      this.socket = new WebSocket(url);
    }

    get readyState(): number {
      return this.socket.readyState;
    }

    get onopen(): WebSocketEventHandler<globalThis.Event> {
      return this.#onopen;
    }

    set onopen(handler: WebSocketEventHandler<globalThis.Event>) {
      this.#onopen = handler;
      this.socket.onopen = handler;
    }

    get onmessage(): WebSocketEventHandler<WebSocketMessage> {
      return this.#onmessage;
    }

    set onmessage(handler: WebSocketEventHandler<WebSocketMessage>) {
      this.#onmessage = handler;
      this.socket.onmessage = handler === null ? null : (event) => handler(event);
    }

    get onclose(): WebSocketEventHandler<WebSocketClose> {
      return this.#onclose;
    }

    set onclose(handler: WebSocketEventHandler<WebSocketClose>) {
      this.#onclose = handler;
      this.socket.onclose = handler === null ? null : (event) => handler(event);
    }

    get onerror(): WebSocketEventHandler<globalThis.Event> {
      return this.#onerror;
    }

    set onerror(handler: WebSocketEventHandler<globalThis.Event>) {
      this.#onerror = handler;
      this.socket.onerror = handler;
    }

    send(data: string): void {
      this.socket.send(data);
    }

    close(): void {
      this.socket.close();
    }
  };
}
