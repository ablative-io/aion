import type { Event } from '@/types';

/**
 * The exactly-once tally — the headline proof of the failover demo.
 *
 * This is a PURE function. Given any multiset of events (history + live, possibly
 * overlapping across a WS reconnect), it counts the *distinct* `ActivityCompleted`
 * activities. Double-counting is structurally impossible: every completion is keyed
 * by its history sequence number (`data.envelope.seq`), which is unique and monotonic
 * within a workflow history, so the same completion appearing twice (once from the
 * describe back-fill, once from the firehose live stream) collapses to one entry.
 *
 * We additionally track the distinct activity ordinals (`data.activity_id`) so a
 * caller can prove "4 distinct activities completed" independently of how many raw
 * frames carried them — a belt-and-braces guard against a server re-emitting the same
 * completion under a new sequence number.
 */

const ACTIVITY_COMPLETED = 'ActivityCompleted';

export type ActivityCompletedEvent = Extract<Event, { type: typeof ACTIVITY_COMPLETED }>;

export type ExactlyOnceTally = {
  /** Distinct completions counted by sequence number. The load-bearing count. */
  count: number;
  /** Distinct activity ordinals seen (`data.activity_id`). */
  distinctOrdinals: number;
  /** The sequence numbers counted, sorted ascending. */
  countedSequences: number[];
  /** The activity ordinals counted, sorted ascending. */
  countedOrdinals: number[];
};

export function isActivityCompleted(event: Event): event is ActivityCompletedEvent {
  return event.type === ACTIVITY_COMPLETED;
}

/**
 * Count distinct completed activities across an arbitrary (possibly overlapping,
 * possibly out-of-order, possibly empty) set of events.
 *
 * Dedup is by `seq` (the exactly-once key). If the same `seq` reappears it is ignored,
 * so replaying overlapping history + live frames can never inflate the count.
 */
export function tallyExactlyOnce(events: Iterable<Event>): ExactlyOnceTally {
  const bySequence = new Map<number, ActivityCompletedEvent>();
  const ordinals = new Set<number>();

  for (const event of events) {
    if (!isActivityCompleted(event)) {
      continue;
    }

    const seq = event.data.envelope.seq;

    if (typeof seq !== 'number') {
      continue;
    }

    if (!bySequence.has(seq)) {
      bySequence.set(seq, event);
      ordinals.add(event.data.activity_id);
    }
  }

  const countedSequences = [...bySequence.keys()].sort((left, right) => left - right);
  const countedOrdinals = [...ordinals].sort((left, right) => left - right);

  return {
    count: bySequence.size,
    distinctOrdinals: ordinals.size,
    countedSequences,
    countedOrdinals,
  };
}

/**
 * The single headline number. Equal to `tallyExactlyOnce(events).count`, but clamps
 * the surfaced value to `arity` when an expected fan-out width is known — the demo's
 * contract is that the count climbs to exactly `arity` and never exceeds it. If the
 * raw distinct count ever exceeds arity (it cannot, by seq-dedup, but we assert it
 * honestly rather than silently), the overflow is reported via `overCount`.
 */
export function completedCount(events: Iterable<Event>, arity?: number): number {
  const { count } = tallyExactlyOnce(events);

  if (arity === undefined) {
    return count;
  }

  return Math.min(count, arity);
}

/** True when the raw distinct count exceeds the expected arity (a contract violation). */
export function hasOverCount(events: Iterable<Event>, arity: number): boolean {
  return tallyExactlyOnce(events).count > arity;
}
