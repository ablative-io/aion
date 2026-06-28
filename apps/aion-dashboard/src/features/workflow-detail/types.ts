import type { Event, EventEnvelope, WorkflowId } from '@/types';

import type { EventVariantFamily, KnownEventType } from './lib/timelineVariants';

export type TimelineEntryKind = 'lifecycle' | 'activity' | 'timer' | 'signal' | 'child' | 'generic';

export type TimelineEvent = Event;

export type TimelineBase = {
  id: string;
  kind: TimelineEntryKind;
  sequence: number;
  recordedAt: string;
  summary: string;
  payload?: unknown;
  events: TimelineEvent[];
  envelope: EventEnvelope;
};

export type LifecycleOutcome = 'started' | 'completed' | 'failed' | 'cancelled' | 'timed-out';

export type LifecycleTimelineEntry = TimelineBase & {
  kind: 'lifecycle';
  outcome: LifecycleOutcome;
  eventType: Event['type'];
};

export type ActivityAttempt = {
  sequence: number;
  recordedAt: string;
  attempt: number;
  error: unknown;
  event: Extract<Event, { type: 'ActivityFailed' }>;
};

export type ActivityTimelineEntry = TimelineBase & {
  kind: 'activity';
  activityId: string;
  activityType: string | null;
  scheduled: Extract<Event, { type: 'ActivityScheduled' }> | null;
  started: Extract<Event, { type: 'ActivityStarted' }> | null;
  completed: Extract<Event, { type: 'ActivityCompleted' }> | null;
  cancelled: Extract<Event, { type: 'ActivityCancelled' }> | null;
  failures: ActivityAttempt[];
  status: 'scheduled' | 'started' | 'completed' | 'failed' | 'cancelled';
};

export type TimerTimelineEntry = TimelineBase & {
  kind: 'timer';
  timerId: string;
  started: Extract<Event, { type: 'TimerStarted' }> | null;
  fired: Extract<Event, { type: 'TimerFired' }> | null;
  cancelled: Extract<Event, { type: 'TimerCancelled' }> | null;
  /**
   * Close event for a `with_timeout` operation that this timer bounded. When
   * present the timer represents a bounded operation (completed-before-deadline
   * or timed-out), not a bare timer.
   */
  withTimeout: Extract<Event, { type: 'WithTimeoutCompleted' }> | null;
  status: 'started' | 'fired' | 'cancelled' | 'completed' | 'timed-out';
};

export type SignalDirection = 'received' | 'sent';

export type SignalTimelineEntry = TimelineBase & {
  kind: 'signal';
  signalName: string;
  direction: SignalDirection;
  /** Target workflow for an outbound `SignalSent`; null for an inbound signal. */
  targetWorkflowId: WorkflowId | null;
  event: Extract<Event, { type: 'SignalReceived' | 'SignalSent' }>;
};

export type ChildWorkflowTimelineEntry = TimelineBase & {
  kind: 'child';
  childWorkflowId: WorkflowId;
  workflowType: string | null;
  started: Extract<Event, { type: 'ChildWorkflowStarted' }> | null;
  completed: Extract<Event, { type: 'ChildWorkflowCompleted' }> | null;
  failed: Extract<Event, { type: 'ChildWorkflowFailed' }> | null;
  cancelled: Extract<Event, { type: 'ChildWorkflowCancelled' }> | null;
  status: 'started' | 'completed' | 'failed' | 'cancelled';
};

/**
 * Typed row for event variants without their own swimlane (continue-as-new,
 * reopened, search-attribute updates, the six schedule lifecycle variants) AND
 * for any future-unknown variant. `family`/`subKind` are populated from the
 * compile-time variant classifier for KNOWN variants so the row labels itself
 * precisely; both are `null` for a genuinely-unknown future variant, which still
 * renders (humanised type label) and never throws. This is what keeps the
 * timeline total over the full 29-variant union.
 */
export type GenericTimelineEntry = TimelineBase & {
  kind: 'generic';
  eventType: Event['type'];
  /** Coarse family from the variant classifier; null for an unknown variant. */
  family: EventVariantFamily | null;
  /** Specific known discriminant; null for an unknown variant. */
  subKind: KnownEventType | null;
  event: Event;
};

export type TimelineEntry =
  | LifecycleTimelineEntry
  | ActivityTimelineEntry
  | TimerTimelineEntry
  | SignalTimelineEntry
  | ChildWorkflowTimelineEntry
  | GenericTimelineEntry;

export type WorkflowDetailProps = {
  workflowId: WorkflowId;
  namespace: string | null;
};
