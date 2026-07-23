import type { Event, PayloadElision } from '@/types';

/**
 * Best-effort payload decoding. Honors invariant 1 (type-erased events): a
 * `Payload` is opaque bytes + a content-type tag. We decode JSON for display and
 * fall back to the raw text — and ultimately the raw value — when we cannot,
 * never assuming a schema the engine does not own and never throwing.
 */

const summaryCache = new WeakMap<object, string>();

export function payloadSummary(payload: unknown): string {
  const elision = findPayloadElision(payload);
  if (elision !== null) {
    return `${formatPayloadKilobytes(elision.size_bytes)} — load`;
  }

  if (typeof payload === 'object' && payload !== null) {
    const cached = summaryCache.get(payload);
    if (cached !== undefined) {
      return cached;
    }
    const summary = summarizeDecodedPayload(decodePayload(payload));
    summaryCache.set(payload, summary);
    return summary;
  }

  return summarizeDecodedPayload(decodePayload(payload));
}

export function decodePayload(payload: unknown): unknown {
  if (!isPayloadBytes(payload)) {
    return payload;
  }

  const text = new TextDecoder().decode(Uint8Array.from(payload.bytes));

  if (payload.content_type !== 'Json') {
    return text;
  }

  try {
    return JSON.parse(text) as unknown;
  } catch {
    return text;
  }
}

export function isPayloadBytes(value: unknown): value is { content_type: string; bytes: number[] } {
  if (typeof value !== 'object' || value === null || !('bytes' in value)) {
    return false;
  }

  const maybePayload = value as { content_type?: unknown; bytes?: unknown };
  return (
    typeof maybePayload.content_type === 'string' &&
    Array.isArray(maybePayload.bytes) &&
    maybePayload.bytes.every((byte) => typeof byte === 'number')
  );
}

export function isPayloadElision(value: unknown): value is PayloadElision {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    return false;
  }
  const candidate = value as { __elided?: unknown; size_bytes?: unknown };
  return (
    candidate.__elided === true &&
    typeof candidate.size_bytes === 'number' &&
    Number.isSafeInteger(candidate.size_bytes) &&
    candidate.size_bytes >= 0
  );
}

export function findPayloadElision(value: unknown): PayloadElision | null {
  if (isPayloadElision(value)) {
    return value;
  }
  if (Array.isArray(value)) {
    for (const item of value) {
      const elision = findPayloadElision(item);
      if (elision !== null) {
        return elision;
      }
    }
    return null;
  }
  if (typeof value !== 'object' || value === null) {
    return null;
  }
  for (const item of Object.values(value)) {
    const elision = findPayloadElision(item);
    if (elision !== null) {
      return elision;
    }
  }
  return null;
}

export function formatPayloadKilobytes(sizeBytes: number): string {
  return `${Math.max(1, Math.ceil(sizeBytes / 1024)).toLocaleString()} KB`;
}

/** Find the raw event that owns a projected payload reference. */
export function eventContainingPayload(
  events: readonly Event[],
  payload: unknown
): Event | undefined {
  return events.find((event) => containsReference(event.data, payload)) ?? events.at(-1);
}

function containsReference(root: unknown, target: unknown): boolean {
  if (root === target) {
    return true;
  }
  if (Array.isArray(root)) {
    return root.some((value) => containsReference(value, target));
  }
  return typeof root === 'object' && root !== null
    ? Object.values(root).some((value) => containsReference(value, target))
    : false;
}

function summarizeDecodedPayload(decodedPayload: unknown): string {
  if (decodedPayload === null) {
    return 'null';
  }
  if (decodedPayload === undefined) {
    return 'none';
  }
  if (
    typeof decodedPayload === 'string' ||
    typeof decodedPayload === 'number' ||
    typeof decodedPayload === 'boolean'
  ) {
    return String(decodedPayload);
  }

  try {
    return JSON.stringify(decodedPayload);
  } catch {
    return String(decodedPayload);
  }
}
