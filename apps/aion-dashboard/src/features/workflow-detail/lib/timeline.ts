import type { Event, EventEnvelope, TimerId } from '@/types';

import type {
  ActivityAttempt,
  ActivityTimelineEntry,
  ChildWorkflowTimelineEntry,
  LifecycleOutcome,
  LifecycleTimelineEntry,
  SignalTimelineEntry,
  TimelineEntry,
  TimerTimelineEntry,
} from '../types';

export function projectTimeline(events: readonly Event[]): TimelineEntry[] {
  const entries: TimelineEntry[] = [];
  const activities = new Map<string, ActivityTimelineEntry>();
  const timers = new Map<string, TimerTimelineEntry>();
  const children = new Map<string, ChildWorkflowTimelineEntry>();

  for (const event of [...events].sort((left, right) => eventSequence(left) - eventSequence(right))) {
    switch (event.type) {
      case 'WorkflowStarted':
      case 'WorkflowCompleted':
      case 'WorkflowFailed':
      case 'WorkflowCancelled':
      case 'WorkflowTimedOut':
        entries.push(lifecycleEntry(event));
        break;
      case 'ActivityScheduled':
      case 'ActivityStarted':
      case 'ActivityCompleted':
      case 'ActivityFailed':
      case 'ActivityCancelled':
        patchActivity(entries, activities, event);
        break;
      case 'TimerStarted':
      case 'TimerFired':
      case 'TimerCancelled':
        patchTimer(entries, timers, event);
        break;
      case 'SignalReceived':
        entries.push(signalEntry(event));
        break;
      case 'ChildWorkflowStarted':
      case 'ChildWorkflowCompleted':
      case 'ChildWorkflowFailed':
      case 'ChildWorkflowCancelled':
        patchChild(entries, children, event);
        break;
      default:
        assertNever(event);
    }
  }

  return entries.sort((left, right) => left.sequence - right.sequence);
}

export function eventSequence(event: Event): number {
  return event.data.envelope.seq;
}

export function eventRecordedAt(event: Event): string {
  return event.data.envelope.recorded_at;
}

export function eventEnvelope(event: Event): EventEnvelope {
  return event.data.envelope;
}

export function payloadSummary(payload: unknown): string {
  if (payload === null) {
    return 'null';
  }

  if (payload === undefined) {
    return 'none';
  }

  if (typeof payload === 'string' || typeof payload === 'number' || typeof payload === 'boolean') {
    return String(payload);
  }

  if (isPayloadBytes(payload)) {
    return `${payload.content_type} payload (${payload.bytes.length} bytes)`;
  }

  try {
    return JSON.stringify(payload);
  } catch {
    return String(payload);
  }
}

function lifecycleEntry(event: Extract<Event, { type: LifecycleType }>): LifecycleTimelineEntry {
  const sequence = eventSequence(event);

  return {
    id: `lifecycle:${event.type}:${sequence}`,
    kind: 'lifecycle',
    sequence,
    recordedAt: eventRecordedAt(event),
    summary: lifecycleSummary(event),
    payload: lifecyclePayload(event),
    events: [event],
    envelope: eventEnvelope(event),
    outcome: lifecycleOutcome(event),
    eventType: event.type,
  };
}

function patchActivity(
  entries: TimelineEntry[],
  map: Map<string, ActivityTimelineEntry>,
  event: Extract<Event, { type: ActivityType }>
): void {
  const activityId = String(event.data.activity_id);
  const entry = map.get(activityId) ?? newActivityEntry(activityId, event);

  if (!map.has(activityId)) {
    map.set(activityId, entry);
    entries.push(entry);
  }

  entry.events.push(event);

  if (event.type === 'ActivityScheduled') {
    entry.scheduled = event;
    entry.activityType = event.data.activity_type;
  } else if (event.type === 'ActivityStarted') {
    entry.started = event;
  } else if (event.type === 'ActivityCompleted') {
    entry.completed = event;
  } else if (event.type === 'ActivityFailed') {
    entry.failures.push(activityAttempt(event));
  } else {
    entry.cancelled = event;
  }

  refreshActivity(entry);
}

function newActivityEntry(
  activityId: string,
  event: Extract<Event, { type: ActivityType }>
): ActivityTimelineEntry {
  return {
    id: `activity:${activityId}`,
    kind: 'activity',
    sequence: eventSequence(event),
    recordedAt: eventRecordedAt(event),
    summary: `Activity ${activityId}`,
    payload: undefined,
    events: [],
    envelope: eventEnvelope(event),
    activityId,
    activityType: null,
    scheduled: null,
    started: null,
    completed: null,
    cancelled: null,
    failures: [],
    status: 'scheduled',
  };
}

function refreshActivity(entry: ActivityTimelineEntry): void {
  const latest = latestEvent(entry.events);
  entry.failures.sort((left, right) => left.sequence - right.sequence);
  entry.status = entry.cancelled
    ? 'cancelled'
    : entry.completed
      ? 'completed'
      : entry.failures.length > 0
        ? 'failed'
        : entry.started
          ? 'started'
          : 'scheduled';
  entry.summary = activitySummary(entry);
  entry.payload = activityPayload(entry);
  entry.recordedAt = latest ? eventRecordedAt(latest) : entry.recordedAt;
}

function activityAttempt(event: Extract<Event, { type: 'ActivityFailed' }>): ActivityAttempt {
  return {
    sequence: eventSequence(event),
    recordedAt: eventRecordedAt(event),
    attempt: event.data.attempt,
    error: event.data.error,
    event,
  };
}

function patchTimer(
  entries: TimelineEntry[],
  map: Map<string, TimerTimelineEntry>,
  event: Extract<Event, { type: TimerType }>
): void {
  const timerId = stringifyTimerId(event.data.timer_id);
  const entry = map.get(timerId) ?? newTimerEntry(timerId, event);

  if (!map.has(timerId)) {
    map.set(timerId, entry);
    entries.push(entry);
  }

  entry.events.push(event);

  if (event.type === 'TimerStarted') {
    entry.started = event;
  } else if (event.type === 'TimerFired') {
    entry.fired = event;
  } else {
    entry.cancelled = event;
  }

  entry.status = entry.cancelled ? 'cancelled' : entry.fired ? 'fired' : 'started';
  entry.summary = `Timer ${timerId} ${entry.status}`;
  entry.payload = entry.started ? { timer_id: timerId, fire_at: entry.started.data.fire_at } : undefined;
  entry.recordedAt = eventRecordedAt(event);
}

function newTimerEntry(timerId: string, event: Extract<Event, { type: TimerType }>): TimerTimelineEntry {
  return {
    id: `timer:${timerId}`,
    kind: 'timer',
    sequence: eventSequence(event),
    recordedAt: eventRecordedAt(event),
    summary: `Timer ${timerId}`,
    payload: undefined,
    events: [],
    envelope: eventEnvelope(event),
    timerId,
    started: null,
    fired: null,
    cancelled: null,
    status: 'started',
  };
}

function signalEntry(event: Extract<Event, { type: 'SignalReceived' }>): SignalTimelineEntry {
  return {
    id: `signal:${event.data.name}:${eventSequence(event)}`,
    kind: 'signal',
    sequence: eventSequence(event),
    recordedAt: eventRecordedAt(event),
    summary: `Signal received: ${event.data.name}`,
    payload: event.data.payload,
    events: [event],
    envelope: eventEnvelope(event),
    signalName: event.data.name,
    event,
  };
}

function patchChild(
  entries: TimelineEntry[],
  map: Map<string, ChildWorkflowTimelineEntry>,
  event: Extract<Event, { type: ChildType }>
): void {
  const childId = event.data.child_workflow_id;
  const entry = map.get(childId) ?? newChildEntry(childId, event);

  if (!map.has(childId)) {
    map.set(childId, entry);
    entries.push(entry);
  }

  entry.events.push(event);

  if (event.type === 'ChildWorkflowStarted') {
    entry.started = event;
    entry.workflowType = event.data.workflow_type;
  } else if (event.type === 'ChildWorkflowCompleted') {
    entry.completed = event;
  } else if (event.type === 'ChildWorkflowFailed') {
    entry.failed = event;
  } else {
    entry.cancelled = event;
  }

  refreshChild(entry);
}

function newChildEntry(
  childId: string,
  event: Extract<Event, { type: ChildType }>
): ChildWorkflowTimelineEntry {
  return {
    id: `child:${childId}`,
    kind: 'child',
    sequence: eventSequence(event),
    recordedAt: eventRecordedAt(event),
    summary: `Child workflow ${childId}`,
    payload: undefined,
    events: [],
    envelope: eventEnvelope(event),
    childWorkflowId: childId,
    workflowType: null,
    started: null,
    completed: null,
    failed: null,
    cancelled: null,
    status: 'started',
  };
}

function refreshChild(entry: ChildWorkflowTimelineEntry): void {
  const latest = latestEvent(entry.events);
  entry.status = entry.cancelled
    ? 'cancelled'
    : entry.failed
      ? 'failed'
      : entry.completed
        ? 'completed'
        : 'started';
  entry.summary = `Child workflow ${entry.workflowType ?? entry.childWorkflowId} ${entry.status}`;
  entry.payload = entry.completed?.data.result ?? entry.failed?.data.error ?? entry.started?.data.input;
  entry.recordedAt = latest ? eventRecordedAt(latest) : entry.recordedAt;
}

function lifecycleOutcome(event: Extract<Event, { type: LifecycleType }>): LifecycleOutcome {
  const outcomes: Record<LifecycleType, LifecycleOutcome> = {
    WorkflowStarted: 'started',
    WorkflowCompleted: 'completed',
    WorkflowFailed: 'failed',
    WorkflowCancelled: 'cancelled',
    WorkflowTimedOut: 'timed-out',
  };

  return outcomes[event.type];
}

function lifecycleSummary(event: Extract<Event, { type: LifecycleType }>): string {
  switch (event.type) {
    case 'WorkflowStarted':
      return `Workflow started: ${event.data.workflow_type}`;
    case 'WorkflowCompleted':
      return `Workflow completed with ${payloadSummary(event.data.result)}`;
    case 'WorkflowFailed':
      return `Workflow failed: ${event.data.error.message}`;
    case 'WorkflowCancelled':
      return `Workflow cancelled: ${event.data.reason}`;
    case 'WorkflowTimedOut':
      return `Workflow timed out after ${event.data.timeout}`;
    default:
      assertNever(event);
  }
}

function lifecyclePayload(event: Extract<Event, { type: LifecycleType }>): unknown {
  switch (event.type) {
    case 'WorkflowStarted':
      return event.data.input;
    case 'WorkflowCompleted':
      return event.data.result;
    case 'WorkflowFailed':
      return event.data.error;
    case 'WorkflowCancelled':
      return { reason: event.data.reason };
    case 'WorkflowTimedOut':
      return { timeout: event.data.timeout };
    default:
      assertNever(event);
  }
}

function activitySummary(entry: ActivityTimelineEntry): string {
  const name = entry.activityType ?? entry.activityId;

  if (entry.status === 'completed') {
    return `Activity ${name} completed after ${entry.failures.length} failed attempt(s)`;
  }

  if (entry.status === 'failed') {
    return `Activity ${name} failed attempt ${entry.failures.at(-1)?.attempt ?? entry.failures.length}`;
  }

  return `Activity ${name} ${entry.status}`;
}

function activityPayload(entry: ActivityTimelineEntry): unknown {
  if (entry.completed) {
    return entry.completed.data.result;
  }

  if (entry.failures.length > 0) {
    return entry.failures.map((failure) => ({ attempt: failure.attempt, error: failure.error }));
  }

  return entry.scheduled?.data.input;
}

function latestEvent(events: readonly Event[]): Event | undefined {
  return events.toSorted((left, right) => eventSequence(left) - eventSequence(right)).at(-1);
}

function stringifyTimerId(timerId: TimerId): string {
  return 'Named' in timerId ? timerId.Named : `anonymous-${timerId.Anonymous}`;
}

function isPayloadBytes(value: unknown): value is { content_type: string; bytes: unknown[] } {
  if (typeof value !== 'object' || value === null || !('bytes' in value)) {
    return false;
  }

  const maybePayload = value as { content_type?: unknown; bytes?: unknown };
  return typeof maybePayload.content_type === 'string' && Array.isArray(maybePayload.bytes);
}

function assertNever(value: never): never {
  throw new Error(`Unhandled event variant: ${JSON.stringify(value)}`);
}

type LifecycleType =
  | 'WorkflowStarted'
  | 'WorkflowCompleted'
  | 'WorkflowFailed'
  | 'WorkflowCancelled'
  | 'WorkflowTimedOut';
type ActivityType =
  | 'ActivityScheduled'
  | 'ActivityStarted'
  | 'ActivityCompleted'
  | 'ActivityFailed'
  | 'ActivityCancelled';
type TimerType = 'TimerStarted' | 'TimerFired' | 'TimerCancelled';
type ChildType =
  | 'ChildWorkflowStarted'
  | 'ChildWorkflowCompleted'
  | 'ChildWorkflowFailed'
  | 'ChildWorkflowCancelled';
