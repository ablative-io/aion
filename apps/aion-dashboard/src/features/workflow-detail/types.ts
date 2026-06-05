import type { Event, EventEnvelope, Payload, WorkflowId } from '@/types';

export type TimelineEntryKind = 'lifecycle' | 'activity' | 'timer' | 'signal' | 'child';

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
  status: 'started' | 'fired' | 'cancelled';
};

export type SignalTimelineEntry = TimelineBase & {
  kind: 'signal';
  signalName: string;
  event: Extract<Event, { type: 'SignalReceived' }>;
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

export type TimelineEntry =
  | LifecycleTimelineEntry
  | ActivityTimelineEntry
  | TimerTimelineEntry
  | SignalTimelineEntry
  | ChildWorkflowTimelineEntry;

export type WorkflowDetailProps = {
  workflowId: WorkflowId;
  namespace: string | null;
};

export type PayloadLike = Payload | unknown;
