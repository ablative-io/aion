import { type AuthOptions, authHeaders } from "./auth.js";
import {
  type AionError,
  InvalidArgumentError,
  ServerError,
  UnavailableError,
  mapTransportError,
  mapWireError,
} from "./errors.js";
import type { SubscribeRequest, SubscribeTransport } from "./stream.js";

/**
 * Path of the server's WebSocket event stream, served by the same axum
 * listener as the HTTP workflow API.
 */
export const EVENT_STREAM_PATH = "/events/stream";

const NORMAL_CLOSURE = 1000;

export interface WebSocketTransportOptions {
  /** The client's configured HTTP endpoint (absolute `http:`/`https:` URL). */
  readonly endpoint: string;
  /** Caller identity forwarded as headers on the upgrade request. */
  readonly auth?: AuthOptions;
}

/**
 * Built-in {@link SubscribeTransport} over Node's global `WebSocket`
 * (requires Node >= 22.4.0 for the documented undici `headers` extension).
 *
 * Protocol, matching the server's `/events/stream` contract:
 * - the websocket URL is derived from the HTTP endpoint by protocol mapping
 *   (`http` -> `ws`, `https` -> `wss`) on the same listener;
 * - auth headers travel on the upgrade request;
 * - the first client frame is the JSON `SubscriptionRequest` (per-workflow,
 *   carrying `resume_from_seq` when the stream machinery passes a cursor);
 * - server frames are `StreamedEvent` JSON, except terminal
 *   `{"error": <WireError>}` frames which are thrown as the mapped taxonomy
 *   error (`lagged` -> {@link UnavailableError} so the resume loop
 *   reconnects; `namespace_denied`/`not_found`/`invalid_input` terminal);
 * - a raw socket drop or any non-1000 close surfaces
 *   {@link UnavailableError}; a 1000 close ends the stream.
 */
export class WebSocketSubscribeTransport implements SubscribeTransport {
  private readonly endpoint: string;
  private readonly auth?: AuthOptions;

  constructor(options: WebSocketTransportOptions) {
    this.endpoint = options.endpoint;
    this.auth = options.auth;
  }

  async *subscribe(request: SubscribeRequest): AsyncIterable<unknown> {
    // Validate the request and derive the URL before opening a socket so
    // invalid input never costs a connection.
    const url = eventStreamUrl(this.endpoint);
    const subscriptionFrame = subscriptionRequestFrame(request);

    const socket = new WebSocket(url, { headers: authHeaders(this.auth) });
    socket.binaryType = "arraybuffer";
    const signals = new SignalQueue();

    socket.addEventListener("open", () => {
      // The first client frame must be the subscription request.
      socket.send(subscriptionFrame);
    });
    socket.addEventListener("message", (event) => {
      signals.push(frameSignal(event.data));
    });
    socket.addEventListener("close", (event) => {
      signals.push({ kind: "closed", code: event.code, reason: event.reason });
    });
    socket.addEventListener("error", (event) => {
      signals.push({ kind: "failed", error: socketError(event) });
    });

    try {
      while (true) {
        const signal = await signals.next();
        switch (signal.kind) {
          case "frame":
            yield decodeStreamFrame(signal.data);
            break;
          case "closed":
            if (signal.code === NORMAL_CLOSURE) {
              return;
            }
            throw new UnavailableError(
              `Event stream websocket closed before the stream ended ` +
                `(code ${signal.code}${
                  signal.reason.length > 0 ? `: ${signal.reason}` : ""
                })`,
              { code: String(signal.code) },
            );
          case "failed":
            throw signal.error;
        }
      }
    } finally {
      // Stream end, terminal error, or the consumer breaking out of the
      // iteration: close the socket cleanly instead of leaking it.
      if (
        socket.readyState === WebSocket.CONNECTING ||
        socket.readyState === WebSocket.OPEN
      ) {
        socket.close(NORMAL_CLOSURE);
      }
    }
  }
}

/**
 * Derives the websocket stream URL from the client's HTTP endpoint. The
 * same axum listener serves `/events/stream`, so this is pure protocol
 * mapping (`http` -> `ws`, `https` -> `wss`), never an invented address.
 */
export function eventStreamUrl(endpoint: string): string {
  let url: URL;
  try {
    url = new URL(endpoint);
  } catch (error) {
    throw new InvalidArgumentError(
      "Event stream endpoint must be an absolute URL",
      { cause: error },
    );
  }
  if (url.protocol === "http:") {
    url.protocol = "ws:";
  } else if (url.protocol === "https:") {
    url.protocol = "wss:";
  } else {
    throw new InvalidArgumentError(
      `Cannot derive a websocket stream URL from a ${url.protocol} endpoint; ` +
        "expected http: or https:",
    );
  }
  url.pathname = `${url.pathname.replace(/\/+$/, "")}${EVENT_STREAM_PATH}`;
  return url.toString();
}

/**
 * Builds the first client frame: the JSON `SubscriptionRequest` with a
 * per-workflow subscription. `resume_from_seq` is the first sequence number
 * wanted (`lastDelivered + 1`); sequence numbers start at 1, so 0 is
 * invalid, and the field is omitted entirely for a live tail.
 */
export function subscriptionRequestFrame(request: SubscribeRequest): string {
  if (
    request.resumeFrom !== undefined &&
    (!Number.isSafeInteger(request.resumeFrom) || request.resumeFrom < 1)
  ) {
    throw new InvalidArgumentError(
      "resumeFrom must be a sequence number >= 1 (the first sequence " +
        `wanted); got ${String(request.resumeFrom)}`,
    );
  }
  const perWorkflow: Record<string, unknown> = {
    namespace: request.namespace,
    workflow_id: { uuid: request.workflowId },
  };
  if (request.resumeFrom !== undefined) {
    perWorkflow.resume_from_seq = request.resumeFrom;
  }
  return JSON.stringify({ per_workflow: perWorkflow });
}

/**
 * Decodes one server text frame. `StreamedEvent` JSON is returned for the
 * stream machinery to decode; a terminal `{"error": <WireError>}` frame is
 * thrown as the mapped taxonomy error.
 */
export function decodeStreamFrame(data: string): unknown {
  let frame: unknown;
  try {
    frame = JSON.parse(data);
  } catch (error) {
    throw new ServerError("Event stream frame is not valid JSON", {
      cause: error,
      body: data,
    });
  }
  if (typeof frame === "object" && frame !== null && "error" in frame) {
    throw mapWireError((frame as { readonly error: unknown }).error);
  }
  return frame;
}

type SocketSignal =
  | { readonly kind: "frame"; readonly data: string }
  | { readonly kind: "closed"; readonly code: number; readonly reason: string }
  | { readonly kind: "failed"; readonly error: AionError };

/** Single-consumer queue bridging websocket events into the generator. */
class SignalQueue {
  private readonly buffered: SocketSignal[] = [];
  private waiter?: (signal: SocketSignal) => void;

  push(signal: SocketSignal): void {
    const waiter = this.waiter;
    if (waiter !== undefined) {
      this.waiter = undefined;
      waiter(signal);
      return;
    }
    this.buffered.push(signal);
  }

  next(): Promise<SocketSignal> {
    const buffered = this.buffered.shift();
    if (buffered !== undefined) {
      return Promise.resolve(buffered);
    }
    return new Promise((resolve) => {
      this.waiter = resolve;
    });
  }
}

function frameSignal(data: unknown): SocketSignal {
  if (typeof data === "string") {
    return { kind: "frame", data };
  }
  if (data instanceof ArrayBuffer) {
    return { kind: "frame", data: new TextDecoder().decode(data) };
  }
  return {
    kind: "failed",
    error: new ServerError(
      "Event stream frame is neither text nor an ArrayBuffer",
    ),
  };
}

function socketError(event: { readonly message: string; readonly error: unknown }): AionError {
  const cause =
    event.error ??
    new Error(
      event.message.length > 0
        ? event.message
        : "Event stream websocket failed",
    );
  return mapTransportError(cause);
}
