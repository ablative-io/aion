import type { Event } from '@/types';

/**
 * The failure context a user reads before deciding to reopen: the step (activity
 * type) that failed and the terminal failure/cancellation reason. Derived from
 * the run's recorded history (the detail view's authoritative source), mirroring
 * the `failed_step`/`failure_reason` fields the list carries from
 * `WorkflowSummary`. Every field is present ONLY when the history records it — a
 * healthy/running/completed run yields `null`, so nothing empty is rendered.
 */
export type FailureContext = {
  /** The activity type whose terminal failure attributes the workflow failure. */
  failedStep: string | null;
  /** The terminal `WorkflowFailed` message, or a `WorkflowCancelled` reason. */
  failureReason: string | null;
};

/**
 * Derive the failure context from a run's history. Returns `null` when the run
 * has no terminal failure/cancellation to explain (running/completed), so the
 * caller renders no empty failure panel.
 */
export function deriveFailureContext(history: readonly Event[]): FailureContext | null {
  const terminal = findTerminal(history);
  if (terminal === null) {
    return null;
  }

  if (terminal.type === 'WorkflowCancelled') {
    const reason = terminal.data.reason;
    return {
      failedStep: null,
      failureReason: reason.length > 0 ? reason : null,
    };
  }

  // WorkflowFailed: the reason is the terminal error message; the failed step is
  // the type of the last terminal-failed activity (best-effort attribution).
  return {
    failedStep: findFailedStep(history),
    failureReason: terminal.data.error.message,
  };
}

type TerminalFailure =
  | Extract<Event, { type: 'WorkflowFailed' }>
  | Extract<Event, { type: 'WorkflowCancelled' }>;

/** The last terminal Failed/Cancelled lifecycle event, or `null`. */
function findTerminal(history: readonly Event[]): TerminalFailure | null {
  let terminal: TerminalFailure | null = null;
  for (const event of history) {
    if (event.type === 'WorkflowFailed' || event.type === 'WorkflowCancelled') {
      terminal = event;
    }
  }
  return terminal;
}

/**
 * The activity type of the last terminally-failed activity — the step to blame.
 * `ActivityScheduled` carries the activity type; we resolve it from the failed
 * activity's earlier schedule event when available. `null` when no schedule
 * recorded the type (the caller then omits the step).
 */
function findFailedStep(history: readonly Event[]): string | null {
  let failedActivityId: number | null = null;
  for (const event of history) {
    if (event.type === 'ActivityFailed' && event.data.error.kind === 'Terminal') {
      failedActivityId = event.data.activity_id;
    }
  }
  if (failedActivityId === null) {
    return null;
  }

  for (const event of history) {
    if (event.type === 'ActivityScheduled' && event.data.activity_id === failedActivityId) {
      return event.data.activity_type;
    }
  }
  return null;
}
