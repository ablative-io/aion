import { expect, test } from 'bun:test';

import {
  classifyKnownEvent,
  type EventVariantFamily,
  KNOWN_EVENT_TYPES,
  type KnownEventType,
} from '../lib/timelineVariants';

test('KNOWN_EVENT_TYPES enumerates exactly the 29 generated variants', () => {
  expect(KNOWN_EVENT_TYPES).toHaveLength(29);
  expect(new Set(KNOWN_EVENT_TYPES).size).toBe(29);
});

test('classifyKnownEvent maps every known variant to a family + sub-kind', () => {
  for (const eventType of KNOWN_EVENT_TYPES) {
    const descriptor = classifyKnownEvent(eventType);
    expect(descriptor).not.toBeNull();
    expect(descriptor?.subKind).toBe(eventType);
  }
});

test('classifyKnownEvent assigns the expected families', () => {
  const expectations: Record<KnownEventType, EventVariantFamily> = {
    WorkflowStarted: 'lifecycle',
    WorkflowCompleted: 'lifecycle',
    WorkflowFailed: 'lifecycle',
    WorkflowCancelled: 'lifecycle',
    WorkflowTimedOut: 'lifecycle',
    WorkflowContinuedAsNew: 'marker',
    WorkflowReopened: 'marker',
    SearchAttributesUpdated: 'marker',
    ActivityScheduled: 'activity',
    ActivityStarted: 'activity',
    ActivityCompleted: 'activity',
    ActivityFailed: 'activity',
    ActivityCancelled: 'activity',
    TimerStarted: 'timer',
    TimerFired: 'timer',
    TimerCancelled: 'timer',
    WithTimeoutCompleted: 'with-timeout',
    SignalReceived: 'signal',
    SignalSent: 'signal',
    ChildWorkflowStarted: 'child',
    ChildWorkflowCompleted: 'child',
    ChildWorkflowFailed: 'child',
    ChildWorkflowCancelled: 'child',
    ScheduleCreated: 'schedule',
    ScheduleUpdated: 'schedule',
    SchedulePaused: 'schedule',
    ScheduleResumed: 'schedule',
    ScheduleDeleted: 'schedule',
    ScheduleTriggered: 'schedule',
  };

  for (const [eventType, family] of Object.entries(expectations)) {
    expect(classifyKnownEvent(eventType)?.family).toBe(family);
  }
});

test('classifyKnownEvent returns null for a genuinely-unknown future variant', () => {
  // A variant the server might add before the wire types are regenerated. The
  // runtime path must degrade safely (null), never throw.
  expect(classifyKnownEvent('SomeFutureUnknownVariant')).toBeNull();
});
