import type { Event, EventEnvelope, TimerId } from '@/types';

import type {
  ActivityAttempt,
  ActivityTimelineEntry,
  ChildWorkflowTimelineEntry,
  GenericTimelineEntry,
  LifecycleOutcome,
  LifecycleTimelineEntry,
  SignalTimelineEntry,
  TimelineEntry,
  TimerTimelineEntry,
} from '../types';
import {
  type LifecycleType,
  lifecycleOutcome,
  lifecyclePayload,
  lifecycleSummary,
  type TerminalLifecycleType,
} from './lifecycle';
import { classifyKnownEvent, genericSummary } from './timelineVariants';

// Stable re-export so existing consumers keep their `../lib/timeline` import path.
export { decodePayload, isPayloadBytes, payloadSummary } from './payload';
export function projectTimeline(events: readonly Event[]): TimelineEntry[] {
  const entries: TimelineEntry[] = [];
  const activities = new Map<string, ActivityTimelineEntry>();
  const timers = new Map<string, TimerTimelineEntry>();
  const children = new Map<string, ChildWorkflowTimelineEntry>();

  for (const event of [...events].sort(
    (left, right) => eventSequence(left) - eventSequence(right)
  )) {
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
      case 'WithTimeoutCompleted':
        patchTimer(entries, timers, event);
        break;
      case 'SignalReceived':
      case 'SignalSent':
        entries.push(signalEntry(event));
        break;
      case 'ChildWorkflowStarted':
      case 'ChildWorkflowCompleted':
      case 'ChildWorkflowFailed':
      case 'ChildWorkflowCancelled':
        patchChild(entries, children, event);
        break;
      // Markers (continue-as-new / reopened / search-attributes) + the six
      // schedule variants render as typed generic rows (classifier family +
      // sub-kind). The default arm also catches any future-unknown variant, so
      // the view never crashes; exhaustiveness over the KNOWN set is enforced at
      // COMPILE time in timelineVariants.ts, never with a runtime throw here.
      default:
        entries.push(genericEntry(event));
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

export function terminalOutcomeForEvents(events: readonly Event[]): LifecycleOutcome | null {
  const terminalEvent = mergeEventsBySequence(events, []).filter(isTerminalWorkflowEvent).at(-1);

  return terminalEvent ? lifecycleOutcome(terminalEvent) : null;
}

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
  } else if (event.type === 'TimerCancelled') {
    entry.cancelled = event;
  } else {
    entry.withTimeout = event;
  }

  entry.status = timerStatus(entry);
  entry.summary = timerSummary(timerId, entry);
  entry.payload = timerPayload(timerId, entry);
  entry.recordedAt = eventRecordedAt(event);
}

function newTimerEntry(
  timerId: string,
  event: Extract<Event, { type: TimerType }>
): TimerTimelineEntry {
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
    withTimeout: null,
    status: 'started',
  };
}

function timerStatus(entry: TimerTimelineEntry): TimerTimelineEntry['status'] {
  if (entry.cancelled) {
    return 'cancelled';
  }

  if (entry.withTimeout) {
    return entry.withTimeout.data.outcome === 'TimedOut' ? 'timed-out' : 'completed';
  }

  return entry.fired ? 'fired' : 'started';
}

function timerSummary(timerId: string, entry: TimerTimelineEntry): string {
  if (entry.withTimeout) {
    return entry.status === 'timed-out'
      ? `Bounded operation on ${timerId} timed out`
      : `Bounded operation on ${timerId} completed before deadline`;
  }

  return `Timer ${timerId} ${entry.status}`;
}

function timerPayload(timerId: string, entry: TimerTimelineEntry): unknown {
  if (entry.withTimeout?.data.result != null) {
    return entry.withTimeout.data.result;
  }

  return entry.started ? { timer_id: timerId, fire_at: entry.started.data.fire_at } : undefined;
}

function signalEntry(
  event: Extract<Event, { type: 'SignalReceived' | 'SignalSent' }>
): SignalTimelineEntry {
  const sent = event.type === 'SignalSent';

  return {
    id: `signal:${sent ? 'sent' : 'received'}:${event.data.name}:${eventSequence(event)}`,
    kind: 'signal',
    sequence: eventSequence(event),
    recordedAt: eventRecordedAt(event),
    summary: sent
      ? `Signal sent: ${event.data.name} → ${event.data.target_workflow_id}`
      : `Signal received: ${event.data.name}`,
    payload: event.data.payload,
    events: [event],
    envelope: eventEnvelope(event),
    signalName: event.data.name,
    direction: sent ? 'sent' : 'received',
    targetWorkflowId: sent ? event.data.target_workflow_id : null,
    event,
  };
}

function genericEntry(event: Event): GenericTimelineEntry {
  const sequence = eventSequence(event);
  const descriptor = classifyKnownEvent(event.type);

  return {
    id: `event:${event.type}:${sequence}`,
    kind: 'generic',
    sequence,
    recordedAt: eventRecordedAt(event),
    summary: genericSummary(event),
    payload: event.data,
    events: [event],
    envelope: eventEnvelope(event),
    eventType: event.type,
    family: descriptor?.family ?? null,
    subKind: descriptor?.subKind ?? null,
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
  entry.payload =
    entry.completed?.data.result ?? entry.failed?.data.error ?? entry.started?.data.input;
  entry.recordedAt = latest ? eventRecordedAt(latest) : entry.recordedAt;
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

type ActivityType = `Activity${'Scheduled' | 'Started' | 'Completed' | 'Failed' | 'Cancelled'}`;
type TimerType = 'TimerStarted' | 'TimerFired' | 'TimerCancelled' | 'WithTimeoutCompleted';
type ChildType = `ChildWorkflow${'Started' | 'Completed' | 'Failed' | 'Cancelled'}`;
