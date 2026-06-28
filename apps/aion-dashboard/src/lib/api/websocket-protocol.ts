import type { Event, Namespace, WorkflowStatus } from '@/types';

import {
  type AionEventSubscriptionFilter,
  type AionSocketError,
  AW_WEBSOCKET_CONTRACT,
  type ConsoleLike,
  type JsonRecord,
  type ResyncContext,
  type SubscriptionRecord,
  type WarningLogger,
  type WebSocketConstructor,
} from './websocket-types';

export const globalConsole = globalThis.console as ConsoleLike;

export const consoleWarn: WarningLogger = (message, error) => {
  globalConsole.warn(message, error);
};

export function buildSubscribeMessage(subscription: SubscriptionRecord): JsonRecord {
  return {
    [AW_WEBSOCKET_CONTRACT.requestKeys.type]: AW_WEBSOCKET_CONTRACT.messageTypes.subscribe,
    [AW_WEBSOCKET_CONTRACT.requestKeys.subscriptionId]: subscription.id,
    [AW_WEBSOCKET_CONTRACT.requestKeys.subscription]: buildSubscriptionRequest(subscription),
  };
}

export function buildUnsubscribeMessage(subscriptionId: string): JsonRecord {
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
            [AW_WEBSOCKET_CONTRACT.requestKeys.workflowType]:
              subscription.filter.workflowType ?? null,
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

export function frameDecodeError(cause: unknown): AionSocketError {
  return {
    kind: 'frame-decode',
    message: 'A live event could not be decoded; the feed may be missing entries.',
    cause,
  };
}

export function reconnectExhaustedError(attempts: number): AionSocketError {
  return {
    kind: 'reconnect-exhausted',
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
    mode: subscription.lastSeenSequence === null ? 'full-refetch' : 'after-sequence',
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

export function matchesSubscription(
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
        : matchesWorkflowType(filter.workflowType, event) &&
            matchesWorkflowStatus(filter.status, event);
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

export function buildWebSocketUrl(baseUrl: string): string {
  const endpoint = AW_WEBSOCKET_CONTRACT.endpoint;

  if (baseUrl.length === 0 && typeof window !== 'undefined') {
    return `${window.location.origin.replace(/^http/, 'ws')}${endpoint}`;
  }

  if (baseUrl.startsWith('ws://') || baseUrl.startsWith('wss://')) {
    return `${baseUrl}${endpoint}`;
  }

  return `${baseUrl.replace(/^http/, 'ws')}${endpoint}`;
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

  return WebSocket as unknown as WebSocketConstructor;
}
