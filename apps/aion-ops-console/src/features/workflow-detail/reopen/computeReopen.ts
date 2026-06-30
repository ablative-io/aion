import type { ActivityId, Event } from '@/types';

import { eventRecordedAt, eventSequence } from '../lib/timeline';

/**
 * How a single activity is treated by a reopen, mirroring engine semantics.
 *
 * - `reused`     — completed with a recorded result; preserved, NOT re-run (green).
 * - `redispatch` — terminal-failed with no later success; re-dispatched on replay
 *                  (amber). This is exactly the `reopened: ActivityId[]` set the
 *                  engine derives and records on `WorkflowReopened`.
 * - `superseded` — a recorded terminal failure that the reset cursor supersedes
 *                  (struck). Always paired with a `redispatch` row for the same
 *                  activity: the failed attempt is what gets superseded.
 */
export type ReopenDisposition = 'reused' | 'redispatch' | 'superseded';

export type ReopenActivityRow = {
  activityId: ActivityId;
  activityType: string | null;
  disposition: ReopenDisposition;
  /** Sequence of the event that determined this disposition (last terminal/completion). */
  decidedAtSeq: number;
  decidedAt: string;
  /** One-based attempt count observed in this run (failures only). */
  attempts: number;
  /** Human-readable terminal failure message for superseded/redispatch rows. */
  failureMessage: string | null;
};

export type ReopenPlan = {
  /** Activity ids that will re-dispatch — the engine's `reopened` set. */
  reopened: ActivityId[];
  /**
   * The reset cursor: the earliest sequence among the superseded terminal
   * failures. The reopened run replays from history and treats every superseded
   * failure at or after this point as a reset, re-resolving to live dispatch.
   * `null` when nothing is reopenable (a clean/completed run).
   */
  cursorResetSeq: number | null;
  /** Per-activity classification, ordered by first-seen sequence. */
  rows: ReopenActivityRow[];
  /** True when at least one activity is terminal-failed with no later success. */
  reopenable: boolean;
};

type ActivityState = {
  activityId: ActivityId;
  activityType: string | null;
  firstSeq: number;
  completed: { seq: number; recordedAt: string } | null;
  /** Latest terminal failure (Terminal kind, no later success). */
  terminalFailure: { seq: number; recordedAt: string; message: string } | null;
  attempts: number;
};

/**
 * Derives the projected effect of reopening a (failed) run from its recorded
 * history — the same way the engine derives the `reopened` set and the reset
 * cursor (see `WorkflowReopened` in the generated wire types and
 * docs/WORKFLOW-REOPEN-DESIGN.md). Pure: no I/O, no engine, no command issued.
 *
 * An activity is re-dispatched iff its last recorded outcome is a Terminal
 * failure with no later `ActivityCompleted`. A completed activity is reused.
 * A Retryable failure with no terminal/success is treated as still-in-flight
 * and left to normal replay (not in the reopened set).
 */
export function computeReopen(events: readonly Event[]): ReopenPlan {
  const states = foldActivityStates(events);

  const rows: ReopenActivityRow[] = [];
  const reopened: ActivityId[] = [];
  let cursorResetSeq: number | null = null;

  const ordered = [...states.values()].sort((left, right) => left.firstSeq - right.firstSeq);

  for (const state of ordered) {
    if (state.completed !== null) {
      rows.push(reusedRow(state));
      continue;
    }

    if (state.terminalFailure !== null) {
      reopened.push(state.activityId);
      cursorResetSeq =
        cursorResetSeq === null
          ? state.terminalFailure.seq
          : Math.min(cursorResetSeq, state.terminalFailure.seq);
      rows.push(supersededRow(state));
      rows.push(redispatchRow(state));
    }
  }

  return {
    reopened,
    cursorResetSeq,
    rows,
    reopenable: reopened.length > 0,
  };
}

/**
 * Folds activity events (in seq order) into per-activity state: completion,
 * latest no-later-success Terminal failure, and the highest attempt seen.
 */
function foldActivityStates(events: readonly Event[]): Map<string, ActivityState> {
  const states = new Map<string, ActivityState>();

  const ordered = [...events].sort((left, right) => eventSequence(left) - eventSequence(right));
  for (const event of ordered) {
    if (event.type === 'ActivityScheduled') {
      ensureState(states, event.data.activity_id, eventSequence(event)).activityType =
        event.data.activity_type;
    } else if (event.type === 'ActivityCompleted') {
      const state = ensureState(states, event.data.activity_id, eventSequence(event));
      state.completed = { seq: eventSequence(event), recordedAt: eventRecordedAt(event) };
      // A success supersedes any earlier failure: the activity is reused, not reopened.
      state.terminalFailure = null;
    } else if (event.type === 'ActivityFailed') {
      applyFailure(ensureState(states, event.data.activity_id, eventSequence(event)), event);
    }
  }

  return states;
}

function applyFailure(
  state: ActivityState,
  event: Extract<Event, { type: 'ActivityFailed' }>
): void {
  state.attempts = Math.max(state.attempts, event.data.attempt);
  if (event.data.error.kind === 'Terminal' && state.completed === null) {
    state.terminalFailure = {
      seq: eventSequence(event),
      recordedAt: eventRecordedAt(event),
      message: event.data.error.message,
    };
  }
}

function ensureState(
  states: Map<string, ActivityState>,
  activityId: ActivityId,
  seq: number
): ActivityState {
  const key = String(activityId);
  const existing = states.get(key);
  if (existing) {
    return existing;
  }
  const created: ActivityState = {
    activityId,
    activityType: null,
    firstSeq: seq,
    completed: null,
    terminalFailure: null,
    attempts: 0,
  };
  states.set(key, created);
  return created;
}

function reusedRow(state: ActivityState): ReopenActivityRow {
  return {
    activityId: state.activityId,
    activityType: state.activityType,
    disposition: 'reused',
    decidedAtSeq: state.completed?.seq ?? state.firstSeq,
    decidedAt: state.completed?.recordedAt ?? '',
    attempts: state.attempts,
    failureMessage: null,
  };
}

function supersededRow(state: ActivityState): ReopenActivityRow {
  return {
    activityId: state.activityId,
    activityType: state.activityType,
    disposition: 'superseded',
    decidedAtSeq: state.terminalFailure?.seq ?? state.firstSeq,
    decidedAt: state.terminalFailure?.recordedAt ?? '',
    attempts: state.attempts,
    failureMessage: state.terminalFailure?.message ?? null,
  };
}

function redispatchRow(state: ActivityState): ReopenActivityRow {
  return {
    activityId: state.activityId,
    activityType: state.activityType,
    disposition: 'redispatch',
    decidedAtSeq: state.terminalFailure?.seq ?? state.firstSeq,
    decidedAt: state.terminalFailure?.recordedAt ?? '',
    attempts: state.attempts,
    failureMessage: state.terminalFailure?.message ?? null,
  };
}
