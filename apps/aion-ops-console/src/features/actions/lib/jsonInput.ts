import type { JsonRecord } from '@/lib/api';

export type ParsedJsonInput = { ok: true; value: JsonRecord } | { ok: false; error: string };

/**
 * Parse the start-workflow input textarea into a JSON object.
 *
 * A blank field is the empty object `{}` — the live engine rejects a start with
 * no payload ("payload is missing"), so a workflow taking no input still needs an
 * empty object on the wire. Non-object JSON (a bare string/number/array) is
 * rejected with a visible message rather than silently coerced.
 */
export function parseJsonInput(raw: string): ParsedJsonInput {
  const trimmed = raw.trim();

  if (trimmed.length === 0) {
    return { ok: true, value: {} };
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch (error) {
    return { ok: false, error: jsonErrorMessage(error) };
  }

  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
    return { ok: false, error: 'Input must be a JSON object, e.g. {"key": "value"}.' };
  }

  return { ok: true, value: parsed as JsonRecord };
}

function jsonErrorMessage(error: unknown): string {
  if (error instanceof Error) {
    return `Invalid JSON: ${error.message}`;
  }

  return 'Invalid JSON.';
}
