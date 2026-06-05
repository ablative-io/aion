import {
  AionClientError,
  ServerError,
  mapTransportError,
  mapWireError,
} from "./errors.js";
import type { WireEnvelope } from "./payload.js";

export interface WorkflowEvent {
  readonly namespace?: string;
  readonly envelope: WireEnvelope;
  readonly seq: number;
  readonly raw: unknown;
}

export interface SubscribeRequest {
  readonly namespace: string;
  readonly workflowId: string;
  readonly runId?: string;
  readonly resumeFrom?: number;
}

export interface SubscribeTransport {
  subscribe(request: SubscribeRequest): AsyncIterable<unknown>;
}

export interface EventStreamOptions {
  readonly transport: SubscribeTransport;
  readonly request: Omit<SubscribeRequest, "resumeFrom">;
  readonly decode?: (frame: unknown) => WorkflowEvent;
  readonly maxReconnects?: number;
}

export function eventStream(
  options: EventStreamOptions,
): AsyncIterable<WorkflowEvent> {
  return new ResumingEventStream(options);
}

class ResumingEventStream implements AsyncIterable<WorkflowEvent> {
  private readonly transport: SubscribeTransport;
  private readonly request: Omit<SubscribeRequest, "resumeFrom">;
  private readonly decode: (frame: unknown) => WorkflowEvent;
  private readonly maxReconnects: number;

  constructor(options: EventStreamOptions) {
    this.transport = options.transport;
    this.request = options.request;
    this.decode = options.decode ?? decodeWorkflowEvent;
    this.maxReconnects = options.maxReconnects ?? Number.POSITIVE_INFINITY;
  }

  async *[Symbol.asyncIterator](): AsyncIterator<WorkflowEvent> {
    let lastDeliveredSeq: number | undefined;
    let reconnects = 0;

    while (true) {
      const resumeFrom =
        lastDeliveredSeq === undefined ? undefined : lastDeliveredSeq + 1;
      const iterable = this.transport.subscribe({
        ...this.request,
        resumeFrom,
      });

      try {
        for await (const frame of iterable) {
          const event = this.decode(frame);
          if (lastDeliveredSeq !== undefined && event.seq <= lastDeliveredSeq) {
            continue;
          }
          lastDeliveredSeq = event.seq;
          reconnects = 0;
          yield event;
        }
        return;
      } catch (error) {
        const mapped = mapStreamError(error);
        if (!isTransient(mapped)) {
          throw mapped;
        }
        reconnects += 1;
        if (reconnects > this.maxReconnects) {
          throw mapped;
        }
      }
    }
  }
}

export function decodeWorkflowEvent(frame: unknown): WorkflowEvent {
  if (isTerminalErrorFrame(frame)) {
    throw mapWireError(frame.error);
  }

  const streamed = asRecord(frame);
  const eventField = streamed?.event;
  const envelope = asWireEnvelope(eventField);
  const payloadValue = asRecord(envelope?.payload);
  const seq = extractSequence(eventField, payloadValue, streamed);

  if (envelope === undefined || seq === undefined) {
    throw new ServerError(
      "Stream frame did not contain an event envelope with a sequence number",
      {
        body: frame,
      },
    );
  }

  return {
    namespace:
      typeof streamed?.namespace === "string"
        ? streamed.namespace
        : envelope.namespace,
    envelope,
    seq,
    raw: frame,
  };
}

function mapStreamError(error: unknown): AionClientError {
  if (error instanceof AionClientError) {
    return error;
  }
  if (isTerminalErrorFrame(error)) {
    return mapWireError(error.error);
  }
  return mapTransportError(error);
}

function isTransient(error: AionClientError): boolean {
  return error.kind === "Unavailable";
}

function isTerminalErrorFrame(
  value: unknown,
): value is { readonly error: unknown } {
  const record = asRecord(value);
  return record !== undefined && "error" in record;
}

function asWireEnvelope(value: unknown): WireEnvelope | undefined {
  const record = asRecord(value);
  const payload = asRecord(record?.payload);
  if (record === undefined || typeof record.namespace !== "string") {
    return undefined;
  }
  return {
    namespace: record.namespace,
    request_id:
      typeof record.request_id === "string" ? record.request_id : undefined,
    payload:
      payload !== undefined &&
      typeof payload.content_type === "string" &&
      Array.isArray(payload.bytes)
        ? {
            content_type: payload.content_type,
            bytes: payload.bytes as readonly number[],
          }
        : undefined,
  };
}

function extractSequence(
  ...candidates: readonly unknown[]
): number | undefined {
  for (const candidate of candidates) {
    const sequence = sequenceFrom(candidate);
    if (sequence !== undefined) {
      return sequence;
    }
  }
  return undefined;
}

function sequenceFrom(value: unknown): number | undefined {
  const record = asRecord(value);
  if (record === undefined) {
    return undefined;
  }
  const possible = [record.seq, record.sequence, record.sequence_number];
  for (const item of possible) {
    if (typeof item === "number" && Number.isSafeInteger(item)) {
      return item;
    }
    if (typeof item === "string") {
      const parsed = Number(item);
      if (Number.isSafeInteger(parsed)) {
        return parsed;
      }
    }
  }
  const nested = asRecord(record.envelope);
  return nested === undefined ? undefined : sequenceFrom(nested);
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null
    ? (value as Record<string, unknown>)
    : undefined;
}
