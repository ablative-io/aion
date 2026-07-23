import type { TimelineEntry } from '../types';
import {
  type OrderedTimelineEvent,
  orderedVisibleEvents,
  type VisibleWorkflow,
} from './timeLayout';

/**
 * Pure prefix-reconstruction for the Execution Scrubber (PLAN S5, VISION §4.2).
 *
 * Scrubbing to `seq=N` shows the workflow state as recorded UP TO sequence N — a
 * faithful slice of the projected timeline, NOT an engine replay (that is Phase-2).
 * The projection is the single source of truth (S3 `projectTimeline`), so we never
 * re-parse raw events here: we slice each already-projected entry down to the
 * events at/below the cut, then re-derive the entry's projected status from that
 * narrower event set. An entry whose every event is later than the cut disappears.
 *
 * `scrubSeq === null` means "live" — the full timeline passes through untouched.
 */

/** Distinct, ascending sequences present in the timeline — the scrubbable stops. */
export function scrubSequences(entries: readonly TimelineEntry[]): number[] {
  const seqs = new Set<number>();

  for (const entry of entries) {
    for (const event of entry.events) {
      seqs.add(event.data.envelope.seq);
    }
    seqs.add(entry.sequence);
  }

  return [...seqs].sort((left, right) => left - right);
}

export type WorkflowCuts = ReadonlyMap<string, number | null>;

/** Continuous shared timestamp → one honest sequence cut per visible workflow. */
export function cutsAtTimestamp(
  workflows: readonly VisibleWorkflow[],
  timestampMs: number
): WorkflowCuts {
  const cuts = new Map<string, number | null>(
    workflows.map((workflow) => [workflow.workflowId, null])
  );

  for (const event of orderedVisibleEvents(workflows)) {
    if (event.recordedAtMs > timestampMs) {
      break;
    }
    const current = cuts.get(event.workflowId);
    cuts.set(
      event.workflowId,
      current === null || current === undefined ? event.sequence : Math.max(current, event.sequence)
    );
  }

  return cuts;
}

/** A stepped scrub includes exactly the globally ordered events through `rank`. */
export function cutsAtGlobalRank(
  workflows: readonly VisibleWorkflow[],
  rank: number
): WorkflowCuts {
  const events = orderedVisibleEvents(workflows);
  return cutsFromOrderedPrefix(workflows, events, snapGlobalRank(rank, events.length));
}

export function snapGlobalRank(rawRank: number, rankCount: number): number {
  if (rankCount <= 0) {
    return 0;
  }
  return Math.min(rankCount - 1, Math.max(0, Math.round(rawRank)));
}

function cutsFromOrderedPrefix(
  workflows: readonly VisibleWorkflow[],
  events: readonly OrderedTimelineEvent[],
  throughRank: number
): WorkflowCuts {
  const cuts = new Map<string, number | null>(
    workflows.map((workflow) => [workflow.workflowId, null])
  );

  for (const event of events.slice(0, throughRank + 1)) {
    const current = cuts.get(event.workflowId);
    cuts.set(
      event.workflowId,
      current === null || current === undefined ? event.sequence : Math.max(current, event.sequence)
    );
  }

  return cuts;
}

/**
 * Filter projected entries to the prefix at/below `scrubSeq`. Each surviving entry
 * keeps only the events recorded at/below the cut; entries with no remaining events
 * are dropped. `entry.sequence` is reset to the first surviving event so lane/rank
 * math (which is seq-driven) stays faithful to the truncated history.
 */
export function prefixUpTo(
  entries: readonly TimelineEntry[],
  scrubSeq: number | null
): TimelineEntry[] {
  if (scrubSeq === null) {
    return [...entries];
  }

  const result: TimelineEntry[] = [];

  for (const entry of entries) {
    const events = entry.events.filter((event) => event.data.envelope.seq <= scrubSeq);

    if (events.length === 0) {
      continue;
    }

    result.push(retainEntry(entry, events, scrubSeq));
  }

  return result.sort((left, right) => left.sequence - right.sequence);
}

/**
 * Rebuild a single entry from its truncated event set. Status-bearing fields are
 * re-derived by clearing any terminal/intermediate event that the cut removed, so
 * a completed activity scrubbed back to before its completion reads as running —
 * exactly the recorded state at that point, with no fabricated values.
 */
function retainEntry(
  entry: TimelineEntry,
  events: TimelineEntry['events'],
  scrubSeq: number
): TimelineEntry {
  const sequence = events[0]?.data.envelope.seq ?? entry.sequence;
  const base = { ...entry, events, sequence } as TimelineEntry;

  switch (base.kind) {
    case 'activity':
      return retainActivity(base, scrubSeq);
    case 'timer':
      return retainTimer(base, scrubSeq);
    case 'child':
      return retainChild(base, scrubSeq);
    default:
      return base;
  }
}

type ActivityEntry = Extract<TimelineEntry, { kind: 'activity' }>;
type TimerEntry = Extract<TimelineEntry, { kind: 'timer' }>;
type ChildEntry = Extract<TimelineEntry, { kind: 'child' }>;

function retainActivity(base: ActivityEntry, scrubSeq: number): ActivityEntry {
  const failures = base.failures.filter((failure) => failure.sequence <= scrubSeq);
  const completed = retain(base.completed, scrubSeq);
  const cancelled = retain(base.cancelled, scrubSeq);
  const started = retain(base.started, scrubSeq);
  const scheduled = retain(base.scheduled, scrubSeq);
  const status = activityStatus(cancelled, completed, failures.length, started);

  return { ...base, scheduled, started, completed, cancelled, failures, status };
}

function activityStatus(
  cancelled: unknown,
  completed: unknown,
  failureCount: number,
  started: unknown
): ActivityEntry['status'] {
  if (cancelled) {
    return 'cancelled';
  }
  if (completed) {
    return 'completed';
  }
  if (failureCount > 0) {
    return 'failed';
  }
  return started ? 'started' : 'scheduled';
}

function retainTimer(base: TimerEntry, scrubSeq: number): TimerEntry {
  const cancelled = retain(base.cancelled, scrubSeq);
  const fired = retain(base.fired, scrubSeq);
  const withTimeout = retain(base.withTimeout, scrubSeq);
  const status = timerStatus(cancelled, withTimeout, fired);

  return { ...base, fired, cancelled, withTimeout, status };
}

function timerStatus(
  cancelled: unknown,
  withTimeout: TimerEntry['withTimeout'],
  fired: unknown
): TimerEntry['status'] {
  if (cancelled) {
    return 'cancelled';
  }
  if (withTimeout) {
    return withTimeout.data.outcome === 'TimedOut' ? 'timed-out' : 'completed';
  }
  return fired ? 'fired' : 'started';
}

function retainChild(base: ChildEntry, scrubSeq: number): ChildEntry {
  const cancelled = retain(base.cancelled, scrubSeq);
  const failed = retain(base.failed, scrubSeq);
  const completed = retain(base.completed, scrubSeq);
  const status = childStatus(cancelled, failed, completed);

  return { ...base, completed, failed, cancelled, status };
}

function childStatus(
  cancelled: unknown,
  failed: unknown,
  completed: unknown
): ChildEntry['status'] {
  if (cancelled) {
    return 'cancelled';
  }
  if (failed) {
    return 'failed';
  }
  return completed ? 'completed' : 'started';
}

/** Keep a correlated event only if it survives the cut; null otherwise. */
function retain<T extends { data: { envelope: { seq: number } } } | null>(
  event: T,
  scrubSeq: number
): T {
  return (event && event.data.envelope.seq <= scrubSeq ? event : null) as T;
}
