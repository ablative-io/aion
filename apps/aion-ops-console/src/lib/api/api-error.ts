export type ServerErrorBody = {
  code?: string;
  message?: string;
};

/** Typed error for a non-success AW REST response. Carries the HTTP status and
 * an optional server error code so callers can surface it to visible state. */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string | null;

  constructor(status: number, message: string, code: string | null = null) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
  }
}
