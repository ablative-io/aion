import type { Event, EventEnvelope } from '@/types';

import type { LifecycleOutcome } from '../types';
import { lifecycleOutcome, type TerminalLifecycleType } from './lifecycle';

export function eventSequence(event: Event): number {
  return event.data.envelope.seq;
}

export function eventRecordedAt(event: Event): string {
  return event.data.envelope.recorded_at;
}

export function eventEnvelope(event: Event): EventEnvelope {
  return event.data.envelope;
}

export function mergeEventsBySequence(history: readonly Event[], live: readonly Event[]): Event[] {
  const eventsBySequence = new Map<number, Event>();

  for (const event of [...history, ...live]) {
    const sequence = eventSequence(event);

    if (!eventsBySequence.has(sequence)) {
      eventsBySequence.set(sequence, event);
    }
  }

  return [...eventsBySequence.values()].sort(
    (left, right) => eventSequence(left) - eventSequence(right)
  );
}

/** Preserve the public contract for unordered and overlapping event collections. */
export function terminalOutcomeForEvents(events: readonly Event[]): LifecycleOutcome | null {
  return terminalOutcomeForMergedEvents(mergeEventsBySequence(events, []));
}

/** Derive terminal state from an already sequence-sorted, deduplicated event array. */
export function terminalOutcomeForMergedEvents(
  mergedEvents: readonly Event[]
): LifecycleOutcome | null {
  // A later `WorkflowReopened` SUPERSEDES an earlier terminal event (AD-012): the
  // run is live again, so the projection must not keep reporting it terminal —
  // stale-terminal here freezes the status badge AND disables the live event
  // subscription while the reopened run is actually executing.
  const lifecycleEvent = mergedEvents
    .filter(
      (event): event is TerminalOrReopenedEvent =>
        isTerminalWorkflowEvent(event) || event.type === 'WorkflowReopened'
    )
    .at(-1);

  if (lifecycleEvent === undefined || lifecycleEvent.type === 'WorkflowReopened') {
    return null;
  }
  return lifecycleOutcome(lifecycleEvent);
}

type TerminalOrReopenedEvent =
  | Extract<Event, { type: TerminalLifecycleType }>
  | Extract<Event, { type: 'WorkflowReopened' }>;

export function isTerminalWorkflowEvent(
  event: Event
): event is Extract<Event, { type: TerminalLifecycleType }> {
  return (
    event.type === 'WorkflowCompleted' ||
    event.type === 'WorkflowFailed' ||
    event.type === 'WorkflowCancelled' ||
    event.type === 'WorkflowTimedOut'
  );
}
