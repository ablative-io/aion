import type { InterventionCapabilities, InterventionOutcome, Namespace } from '@/types';

import { ApiError } from './api-error';
import type { AttemptCapabilities, Capabilities, WhoAmIResponse } from './client-types';

/** Raw `/workflows/attempts` envelope (server snake_case). */
export type AttemptsResponseBody = {
  attempts?: unknown;
};

/** Raw `/workflows/intervene` envelope (server snake_case). */
export type InterveneResponseBody = {
  outcome?: unknown;
};

/** Normalize `/whoami` conservatively so malformed fields never grant an affordance. */
export function normalizeCapabilities(response: WhoAmIResponse): Capabilities {
  const namespaces = Array.isArray(response.namespaces)
    ? response.namespaces.filter((entry): entry is Namespace => typeof entry === 'string')
    : [];

  return {
    subject: typeof response.subject === 'string' ? response.subject : 'anonymous',
    authEnabled: response.auth_enabled !== false,
    deployGranted: response.deploy_granted === true,
    allNamespaces: response.all_namespaces === true,
    namespaces,
  };
}

/** Normalize only addressable attempt rows; malformed envelopes are contract failures. */
export function normalizeAttempts(response: AttemptsResponseBody): AttemptCapabilities[] {
  const rows = response.attempts;
  if (!Array.isArray(rows)) {
    throw new ApiError(200, 'workflows/attempts response missing an attempts array');
  }

  const attempts: AttemptCapabilities[] = [];
  for (const row of rows) {
    const attempt = readAttemptRow(row);
    if (attempt !== null) {
      attempts.push(attempt);
    }
  }
  return attempts;
}

function readAttemptRow(row: unknown): AttemptCapabilities | null {
  if (typeof row !== 'object' || row === null) {
    return null;
  }
  const record = row as Record<string, unknown>;
  const activityId = record.activity_id;
  const attempt = record.attempt;
  const capabilities = record.capabilities;
  if (
    typeof activityId !== 'number' ||
    typeof attempt !== 'number' ||
    !isCapabilities(capabilities)
  ) {
    return null;
  }
  return { activityId, attempt, capabilities };
}

function isCapabilities(value: unknown): value is InterventionCapabilities {
  return (
    typeof value === 'object' &&
    value !== null &&
    Array.isArray((value as { supported?: unknown }).supported)
  );
}

/** Read the neutral intervention result or reject a malformed success envelope. */
export function readInterventionOutcome(response: InterveneResponseBody): InterventionOutcome {
  const outcome = response.outcome;
  if (
    typeof outcome === 'object' &&
    outcome !== null &&
    typeof (outcome as { outcome?: unknown }).outcome === 'string'
  ) {
    return outcome as InterventionOutcome;
  }
  throw new ApiError(200, 'workflows/intervene response missing a neutral outcome');
}
