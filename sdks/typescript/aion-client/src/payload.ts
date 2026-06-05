import { InvalidArgumentError, ServerError } from "./errors.js";

export const JSON_CONTENT_TYPE = "application/json";

export interface Payload {
  readonly content_type: string;
  readonly bytes: readonly number[];
}

export interface WireEnvelope {
  readonly namespace: string;
  readonly request_id?: string;
  readonly payload?: Payload;
}

export function rawPayload(
  bytes: Uint8Array | readonly number[],
  contentType: string,
): Payload {
  return {
    content_type: contentType,
    bytes: Array.from(bytes),
  };
}

export function toPayload<T>(value: T): Payload {
  try {
    const json = JSON.stringify(value);
    if (json === undefined) {
      throw new TypeError("Value cannot be represented as JSON");
    }
    return rawPayload(new TextEncoder().encode(json), JSON_CONTENT_TYPE);
  } catch (error) {
    throw new InvalidArgumentError("Failed to serialize payload as JSON", {
      cause: error,
    });
  }
}

export function fromPayload<T>(payload: Payload): T {
  if (payload.content_type !== JSON_CONTENT_TYPE) {
    throw new InvalidArgumentError(
      `Cannot decode ${payload.content_type} payload as JSON`,
    );
  }

  try {
    const text = new TextDecoder().decode(payloadBytes(payload));
    return JSON.parse(text) as T;
  } catch (error) {
    throw new ServerError("Failed to decode JSON payload", { cause: error });
  }
}

export function payloadBytes(payload: Payload): Uint8Array {
  return Uint8Array.from(payload.bytes);
}

export function isPayload(value: unknown): value is Payload {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const candidate = value as {
    readonly content_type?: unknown;
    readonly bytes?: unknown;
  };
  return (
    typeof candidate.content_type === "string" && isByteArray(candidate.bytes)
  );
}

function isByteArray(value: unknown): value is readonly number[] {
  return (
    Array.isArray(value) &&
    value.every((byte) => Number.isInteger(byte) && byte >= 0 && byte <= 255)
  );
}
