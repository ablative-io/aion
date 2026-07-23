import type { ActivityEvent } from '@/types';

export type DecodedToolCall = {
  type: 'call';
  callId: string;
  name: string;
  arguments: unknown;
  kind: string | null;
  event: ActivityEvent;
};

export type DecodedToolResult = {
  type: 'result';
  callId: string;
  name: string;
  output: unknown;
  durationMs: number | null;
  event: ActivityEvent;
};

export type DecodedToolEvent = DecodedToolCall | DecodedToolResult;

export type DecodedToolDelta = {
  itemId: string;
  callId: string | null;
  name: string | null;
  argumentsDelta: string;
  kind: string | null;
};

export type StreamKind = 'text' | 'thinking';

/**
 * Decode the tool-bearing norn notification while continuing to support the
 * server's classified ActivityEvent projection. Raw notifications may be the
 * params directly (`source` is the method) or a complete JSON-RPC frame.
 */
export function decodeToolEvent(event: ActivityEvent): DecodedToolEvent | null {
  if (event.kind.kind === 'ToolCall') {
    return {
      type: 'call',
      callId: event.kind.call_id,
      name: event.kind.tool,
      arguments: parseArguments(event.kind.input),
      kind: null,
      event,
    };
  }
  if (event.kind.kind === 'ToolResult') {
    return {
      type: 'result',
      callId: event.kind.call_id,
      name: '',
      output: event.kind.output,
      durationMs: null,
      event,
    };
  }

  const notification = rawNotification(event);
  if (notification === null) {
    return null;
  }
  const { method, params } = notification;
  if (method === 'event/toolCall' && params.type === 'tool_call') {
    return {
      type: 'call',
      callId: stringField(params, 'call_id'),
      name: stringField(params, 'name'),
      arguments: parseArguments(params.arguments),
      kind: nullableStringField(params, 'kind'),
      event,
    };
  }
  if (method === 'event/toolResult' && params.type === 'tool_result') {
    return {
      type: 'result',
      callId: stringField(params, 'tool_call_id'),
      name: stringField(params, 'tool_name'),
      output: params.output,
      durationMs: numberField(params, 'duration_ms'),
      event,
    };
  }
  return null;
}

/** Decode an unclassified live tool-argument notification. */
export function decodeToolDelta(event: ActivityEvent): DecodedToolDelta | null {
  const notification = rawNotification(event);
  if (
    notification === null ||
    notification.method !== 'event/progress' ||
    notification.params.type !== 'tool_call_delta'
  ) {
    return null;
  }
  const params = notification.params;
  return {
    itemId: stringField(params, 'item_id'),
    callId: nullableStringField(params, 'call_id'),
    name: nullableStringField(params, 'name'),
    argumentsDelta: stringField(params, 'arguments_delta'),
    kind: nullableStringField(params, 'kind'),
  };
}

/**
 * The classified event intentionally retains only message id + text. Norn's
 * reasoning item ids use the `rs_` namespace; raw frames retain the native type
 * and are preferred when available.
 */
export function streamKind(event: ActivityEvent): StreamKind {
  const notification = rawNotification(event);
  if (notification?.params.type === 'thinking_delta') {
    return 'thinking';
  }
  if (event.kind.kind === 'Delta' && event.kind.message_id.startsWith('rs_')) {
    return 'thinking';
  }
  return 'text';
}

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

export function prettyJson(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2) ?? String(value);
  } catch {
    return String(value);
  }
}

function parseArguments(value: unknown): unknown {
  if (typeof value !== 'string') {
    return value;
  }
  try {
    return JSON.parse(value) as unknown;
  } catch {
    return value;
  }
}

function rawNotification(
  event: ActivityEvent
): { method: string; params: Record<string, unknown> } | null {
  if (event.kind.kind !== 'Raw' || !isRecord(event.kind.value)) {
    return null;
  }
  const value = event.kind.value;
  if (typeof value.method === 'string' && isRecord(value.params)) {
    return { method: value.method, params: value.params };
  }
  if (event.kind.source.startsWith('event/')) {
    return { method: event.kind.source, params: value };
  }
  return null;
}

function stringField(value: Record<string, unknown>, key: string): string {
  return typeof value[key] === 'string' ? value[key] : '';
}

function nullableStringField(value: Record<string, unknown>, key: string): string | null {
  return typeof value[key] === 'string' ? value[key] : null;
}

function numberField(value: Record<string, unknown>, key: string): number | null {
  return typeof value[key] === 'number' && Number.isFinite(value[key]) ? value[key] : null;
}
