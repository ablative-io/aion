export const aionErrorKinds = [
  "NotFound",
  "AlreadyExists",
  "QueryFailed",
  "QueryTimeout",
  "Cancelled",
  "Unavailable",
  "Unauthenticated",
  "NamespaceDenied",
  "InvalidArgument",
  "Server",
] as const;

export type AionErrorKind = (typeof aionErrorKinds)[number];

export interface WireErrorLike {
  readonly code?: unknown;
  readonly message?: unknown;
  readonly detail?: unknown;
}

export interface ErrorDetail {
  readonly status?: number;
  readonly code?: string;
  readonly cause?: unknown;
  readonly body?: unknown;
}

export abstract class AionClientError extends Error {
  abstract readonly kind: AionErrorKind;
  readonly detail?: ErrorDetail;

  constructor(message: string, detail?: ErrorDetail) {
    super(message);
    this.name = new.target.name;
    this.detail = detail;
  }
}

export class NotFoundError extends AionClientError {
  readonly kind = "NotFound" as const;
}

export class AlreadyExistsError extends AionClientError {
  readonly kind = "AlreadyExists" as const;
}

export class QueryFailedError extends AionClientError {
  readonly kind = "QueryFailed" as const;
}

export class QueryTimeoutError extends AionClientError {
  readonly kind = "QueryTimeout" as const;
}

export class CancelledError extends AionClientError {
  readonly kind = "Cancelled" as const;
}

export class UnavailableError extends AionClientError {
  readonly kind = "Unavailable" as const;
}

export class UnauthenticatedError extends AionClientError {
  readonly kind = "Unauthenticated" as const;
}

/**
 * The caller's credential was accepted, but the caller has no grant for the
 * requested namespace. Workflow-level invisibility within a granted
 * namespace (a workflow that does not exist or is owned by another
 * namespace) surfaces as {@link NotFoundError} instead, so the response
 * never leaks whether the workflow exists. Maps from the `namespace_denied`
 * wire error code (numeric 2) and HTTP 403. Distinct from
 * {@link UnauthenticatedError} (credential failure, HTTP 401) and from
 * {@link InvalidArgumentError}; not retryable until grants change.
 */
export class NamespaceDeniedError extends AionClientError {
  readonly kind = "NamespaceDenied" as const;
}

export class InvalidArgumentError extends AionClientError {
  readonly kind = "InvalidArgument" as const;
}

export class ServerError extends AionClientError {
  readonly kind = "Server" as const;
}

export type AionError =
  | NotFoundError
  | AlreadyExistsError
  | QueryFailedError
  | QueryTimeoutError
  | CancelledError
  | UnavailableError
  | UnauthenticatedError
  | NamespaceDeniedError
  | InvalidArgumentError
  | ServerError;

export function isAionClientError(error: unknown): error is AionError {
  return error instanceof AionClientError;
}

export function assertNever(value: never): never {
  throw new ServerError("Unhandled Aion error variant", { cause: value });
}

export function mapTransportError(error: unknown): AionError {
  if (isAionClientError(error)) {
    return error;
  }
  return new UnavailableError(
    errorMessage(error, "Aion server is unavailable"),
    { cause: error },
  );
}

export async function mapHttpResponseError(
  response: Response,
): Promise<AionError> {
  const body = await readErrorBody(response);
  const message =
    wireMessage(body) ?? `${response.status} ${response.statusText}`.trim();
  const detail: ErrorDetail = { status: response.status, body };

  switch (response.status) {
    case 400:
    case 412:
      return new InvalidArgumentError(message, detail);
    case 401:
      return new UnauthenticatedError(message, detail);
    case 403:
      return new NamespaceDeniedError(message, detail);
    case 404:
      return new NotFoundError(message, detail);
    case 408:
      return new QueryTimeoutError(message, detail);
    case 409:
      // A 409 carrying the `sequence_conflict` wire code is a server
      // double-writer bug, not a caller-visible conflict; defer to the wire
      // mapping so it surfaces as ServerError.
      return normalizeWireCode(asWireError(body)?.code) === "sequence_conflict"
        ? mapWireError(body, detail)
        : new AlreadyExistsError(message, detail);
    case 429:
      return new UnavailableError(message, detail);
    default:
      return mapWireError(body, detail);
  }
}

export function mapWireError(
  error: unknown,
  detail: ErrorDetail = {},
): AionError {
  if (isAionClientError(error)) {
    return error;
  }

  const wire = asWireError(error);
  const message = wireMessage(wire) ?? "Aion server returned an error";
  const code = normalizeWireCode(wire?.code);
  const mergedDetail: ErrorDetail = { ...detail, code: code ?? detail.code };

  switch (code) {
    case "not_found":
      return new NotFoundError(message, mergedDetail);
    case "already_exists":
    case "idempotency_conflict":
      return new AlreadyExistsError(message, mergedDetail);
    case "query_failed":
      return new QueryFailedError(message, mergedDetail);
    case "query_timeout":
      return new QueryTimeoutError(message, mergedDetail);
    case "cancelled":
      return new CancelledError(message, mergedDetail);
    case "unavailable":
    case "lagged":
      return new UnavailableError(message, mergedDetail);
    case "unauthenticated":
      return new UnauthenticatedError(message, mergedDetail);
    case "namespace_denied":
      return new NamespaceDeniedError(message, mergedDetail);
    case "invalid_argument":
    case "invalid_input":
    case "unknown_query":
    case "not_running":
      return new InvalidArgumentError(message, mergedDetail);
    // `sequence_conflict` signals an internal double-writer bug on the
    // server, never a caller-visible idempotency outcome, so it maps to
    // ServerError alongside `backend` and any unknown code.
    case "sequence_conflict":
    case "backend":
    default:
      return new ServerError(message, mergedDetail);
  }
}

function asWireError(value: unknown): WireErrorLike | undefined {
  if (typeof value === "object" && value !== null) {
    return value as WireErrorLike;
  }
  return undefined;
}

function wireMessage(value: unknown): string | undefined {
  const wire = asWireError(value);
  return typeof wire?.message === "string" && wire.message.length > 0
    ? wire.message
    : undefined;
}

function errorMessage(error: unknown, fallback: string): string {
  if (error instanceof Error && error.message.length > 0) {
    return error.message;
  }
  if (typeof error === "string" && error.length > 0) {
    return error;
  }
  return fallback;
}

async function readErrorBody(response: Response): Promise<unknown> {
  const text = await response.text();
  if (text.length === 0) {
    return undefined;
  }
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return { message: text };
  }
}

function normalizeWireCode(code: unknown): string | undefined {
  if (typeof code === "number") {
    return numericWireCode(code);
  }
  if (typeof code !== "string") {
    return undefined;
  }
  return code
    .replace(/^WIRE_ERROR_CODE_/, "")
    .replace(/^wire_error_code_/, "")
    .toLowerCase();
}

function numericWireCode(code: number): string | undefined {
  switch (code) {
    case 1:
      return "not_found";
    case 2:
      return "namespace_denied";
    case 3:
      return "sequence_conflict";
    case 4:
      return "unknown_query";
    case 5:
      return "query_timeout";
    case 6:
      return "not_running";
    case 7:
      return "lagged";
    case 8:
      return "invalid_input";
    case 9:
      return "backend";
    default:
      return undefined;
  }
}
