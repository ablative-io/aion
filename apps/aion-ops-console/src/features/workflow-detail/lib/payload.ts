/**
 * Best-effort payload decoding. Honors invariant 1 (type-erased events): a
 * `Payload` is opaque bytes + a content-type tag. We decode JSON for display and
 * fall back to the raw text — and ultimately the raw value — when we cannot,
 * never assuming a schema the engine does not own and never throwing.
 */

export function payloadSummary(payload: unknown): string {
  const decodedPayload = decodePayload(payload);

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
