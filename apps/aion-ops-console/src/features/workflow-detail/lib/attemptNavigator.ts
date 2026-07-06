import type { Event } from '@/types';

import type { ActivityTimelineEntry, TimelineEntry } from '../types';

/**
 * The DURABLE attempt navigator (NOI post-mortem fix).
 *
 * The old attempts panel enumerated only the LIVE intervenable attempts
 * (`POST /workflows/attempts`), which returns empty for a terminal run — so a
 * failed/finished workflow's attempts went blank and their (durable) transcripts
 * were unreachable. This module instead derives the selectable attempt list from
 * the projected timeline (durable history + live), so every attempt a run ever ran
 * stays selectable after the run ends.
 *
 * Each row is one `(activityId, attempt)` — the STABLE identity across the whole
 * lifecycle (`ActivityStarted/Completed/Failed/Cancelled` all carry the same
 * one-based `attempt`). Rows and the current selection are keyed by that durable
 * identity, NEVER by array index (indexes shift as live events extend the
 * timeline; the durable key survives refresh and post-mortem). `attempt: 0` is the
 * legacy/unknown sentinel (never a real attempt) — it is labelled distinctly and
 * remains selectable.
 */

/** Terminal state of one `(activityId, attempt)`, derived from its terminal event. */
export type AttemptStatus = 'started' | 'completed' | 'failed' | 'cancelled';

export type AttemptNavigatorRow = {
  /**
   * Durable identity `${activityId}:${attempt}`. Survives refresh + post-mortem
   * and is the sole selection key (never an array index).
   */
  key: string;
  activityId: string;
  /** Human activity type when the schedule is known; otherwise `null`. */
  activityType: string | null;
  /** One-based attempt number; `0` is the legacy/unknown sentinel. */
  attempt: number;
  /** True for the `attempt: 0` legacy sentinel (still selectable, labelled apart). */
  isLegacy: boolean;
  status: AttemptStatus;
};

/** The one true selection key for an `(activityId, attempt)` pair. */
export function attemptRowKey(activityId: string | number, attempt: number): string {
  return `${activityId}:${attempt}`;
}

/** The attempt-bearing terminal/start events for one activity (all carry `attempt`). */
type AttemptEventType =
  | 'ActivityStarted'
  | 'ActivityCompleted'
  | 'ActivityFailed'
  | 'ActivityCancelled';

const ATTEMPT_EVENT_TYPES = new Set<Event['type']>([
  'ActivityStarted',
  'ActivityCompleted',
  'ActivityFailed',
  'ActivityCancelled',
]);

function isAttemptEvent(event: Event): event is Extract<Event, { type: AttemptEventType }> {
  return ATTEMPT_EVENT_TYPES.has(event.type);
}

/** Accumulated presence of each terminal for one attempt (order-independent). */
type AttemptSlot = {
  started: boolean;
  completed: boolean;
  failed: boolean;
  cancelled: boolean;
};

function emptySlot(): AttemptSlot {
  return { started: false, completed: false, failed: false, cancelled: false };
}

/**
 * A completed/cancelled attempt takes precedence over a failure, which takes
 * precedence over a bare start. (An attempt has exactly one real terminal, but we
 * resolve defensively so a malformed group never yields an ambiguous status.)
 */
function slotStatus(slot: AttemptSlot): AttemptStatus {
  if (slot.completed) {
    return 'completed';
  }
  if (slot.cancelled) {
    return 'cancelled';
  }
  if (slot.failed) {
    return 'failed';
  }
  return 'started';
}

/**
 * Group every activity's attempt-bearing events by their one-based `attempt` and
 * emit one navigator row per `(activityId, attempt)`, ordered by activity then
 * attempt. Scheduled-only activities (no attempt has run yet) yield no rows —
 * there is no transcript to observe until an attempt starts.
 */
export function deriveAttemptNavigator(timeline: readonly TimelineEntry[]): AttemptNavigatorRow[] {
  const rows: AttemptNavigatorRow[] = [];

  for (const entry of timeline) {
    if (entry.kind === 'activity') {
      rows.push(...activityRows(entry));
    }
  }

  return rows.sort(compareRows);
}

/** Group one activity's attempt-bearing events by `attempt` into navigator rows. */
function activityRows(entry: ActivityTimelineEntry): AttemptNavigatorRow[] {
  const byAttempt = new Map<number, AttemptSlot>();

  for (const event of entry.events) {
    if (isAttemptEvent(event)) {
      applyAttemptEvent(byAttempt, event);
    }
  }

  return [...byAttempt].map(([attempt, slot]) => ({
    key: attemptRowKey(entry.activityId, attempt),
    activityId: entry.activityId,
    activityType: entry.activityType,
    attempt,
    isLegacy: attempt === 0,
    status: slotStatus(slot),
  }));
}

/** Record one attempt-bearing event's terminal presence in its attempt's slot. */
function applyAttemptEvent(
  byAttempt: Map<number, AttemptSlot>,
  event: Extract<Event, { type: AttemptEventType }>
): void {
  const slot = byAttempt.get(event.data.attempt) ?? emptySlot();
  if (event.type === 'ActivityStarted') {
    slot.started = true;
  } else if (event.type === 'ActivityCompleted') {
    slot.completed = true;
  } else if (event.type === 'ActivityFailed') {
    slot.failed = true;
  } else {
    slot.cancelled = true;
  }
  byAttempt.set(event.data.attempt, slot);
}

/** Ascending by activity id (numeric when possible) then attempt — the list order. */
function compareRows(left: AttemptNavigatorRow, right: AttemptNavigatorRow): number {
  const leftId = Number(left.activityId);
  const rightId = Number(right.activityId);
  const bothNumeric = !Number.isNaN(leftId) && !Number.isNaN(rightId);
  const byActivity = bothNumeric
    ? leftId - rightId
    : left.activityId.localeCompare(right.activityId);
  return byActivity || left.attempt - right.attempt;
}
