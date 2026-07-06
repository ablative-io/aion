import { expect, test } from 'bun:test';

import type { Event, Payload } from '@/types';
import type { ScheduleConfig } from '@/types/generated';

import {
  mergeEventsBySequence,
  payloadSummary,
  projectTimeline,
  terminalOutcomeForEvents,
} from '../lib/timeline';
import { KNOWN_EVENT_TYPES } from '../lib/timelineVariants';
import type { TimelineEntry } from '../types';

const workflowId = 'workflow-1';
const jsonPayload: Payload = { content_type: 'Json', bytes: [123, 125] };
const scheduleConfig: ScheduleConfig = {
  trigger: { Cron: { expression: '0 * * * *' } },
  overlap_policy: 'Skip',
  catch_up_policy: 'One',
  workflow_type: 'nightly',
  input: jsonPayload,
  search_attributes: {},
};

test('projectTimeline orders by sequence and groups logical lifecycle families', () => {
  const timeline = projectTimeline([
    activityCompleted(5),
    workflowStarted(1),
    activityStarted(3),
    activityScheduled(2),
    signalReceived(4),
  ]);

  expect(timeline.map((entry) => entry.sequence)).toEqual([1, 2, 4]);
  expect(timeline.map((entry) => entry.kind)).toEqual(['lifecycle', 'activity', 'signal']);
  expect(timeline[1]?.events.map((event) => event.type)).toEqual([
    'ActivityScheduled',
    'ActivityStarted',
    'ActivityCompleted',
  ]);
});

test('payloadSummary decodes generated JSON payload bytes', () => {
  expect(payloadSummary(jsonPayload)).toBe('{}');
});

test('mergeEventsBySequence de-duplicates history/live overlap and exposes terminal outcome', () => {
  const merged = mergeEventsBySequence(
    [workflowStarted(1), activityScheduled(2), activityStarted(3)],
    [activityStarted(3), workflowCompleted(5), signalReceived(4)]
  );

  expect(merged.map((event) => event.data.envelope.seq)).toEqual([1, 2, 3, 4, 5]);
  expect(merged.filter((event) => event.data.envelope.seq === 3)).toHaveLength(1);
  expect(terminalOutcomeForEvents(merged)).toBe('completed');
  expect(projectTimeline(merged).at(-1)?.summary).toContain('Workflow completed');
});

test('projectTimeline records activity retry history and child workflow links', () => {
  const timeline = projectTimeline([
    activityScheduled(1),
    activityFailed(2, 1),
    activityFailed(3, 2),
    activityCompleted(4),
    childStarted(5),
    childCompleted(6),
  ]);
  const activity = timeline.find((entry) => entry.kind === 'activity');
  const child = timeline.find((entry) => entry.kind === 'child');

  expect(activity?.kind).toBe('activity');

  if (activity?.kind !== 'activity') {
    throw new Error('expected an activity entry');
  }

  expect(activity.failures.map((failure) => failure.attempt)).toEqual([1, 2]);
  expect(activity.status).toBe('completed');
  expect(activity.events).toHaveLength(4);
  expect(child?.kind).toBe('child');

  if (child?.kind !== 'child') {
    throw new Error('expected a child entry');
  }

  expect(child.childWorkflowId).toBe('child-1');
  expect(child.status).toBe('completed');
});

function envelope(seq: number) {
  return { seq, recorded_at: `2026-06-05T20:00:0${seq}Z`, workflow_id: workflowId };
}

function workflowStarted(seq: number): Event {
  return {
    type: 'WorkflowStarted',
    data: {
      envelope: envelope(seq),
      workflow_type: 'checkout',
      input: jsonPayload,
      run_id: '00000000-0000-0000-0000-0000000000a1',
      parent_run_id: null,
      package_version: '1.0.0',
    },
  };
}

function workflowCompleted(seq: number): Event {
  return {
    type: 'WorkflowCompleted',
    data: { envelope: envelope(seq), result: jsonPayload },
  };
}

function activityScheduled(seq: number): Event {
  return {
    type: 'ActivityScheduled',
    data: {
      envelope: envelope(seq),
      activity_id: 7,
      activity_type: 'charge-card',
      input: jsonPayload,
      task_queue: 'default',
      node: null,
    },
  };
}

function activityStarted(seq: number): Event {
  return { type: 'ActivityStarted', data: { envelope: envelope(seq), activity_id: 7, attempt: 1 } };
}

function activityCompleted(seq: number): Event {
  return {
    type: 'ActivityCompleted',
    data: { envelope: envelope(seq), activity_id: 7, attempt: 1, result: jsonPayload },
  };
}

function activityFailed(seq: number, attempt: number): Event {
  return {
    type: 'ActivityFailed',
    data: {
      envelope: envelope(seq),
      activity_id: 7,
      attempt,
      error: { kind: 'Retryable', message: `attempt ${attempt} failed`, details: null },
    },
  };
}

function signalReceived(seq: number): Event {
  return {
    type: 'SignalReceived',
    data: { envelope: envelope(seq), name: 'payment-updated', payload: jsonPayload },
  };
}

function childStarted(seq: number): Event {
  return {
    type: 'ChildWorkflowStarted',
    data: {
      envelope: envelope(seq),
      child_workflow_id: 'child-1',
      workflow_type: 'receipt',
      input: jsonPayload,
      package_version: '1.0.0',
    },
  };
}

function childCompleted(seq: number): Event {
  return {
    type: 'ChildWorkflowCompleted',
    data: { envelope: envelope(seq), child_workflow_id: 'child-1', result: jsonPayload },
  };
}

// --- 29-variant exhaustiveness coverage -----------------------------------

/** One concrete event for each of the 29 generated variants, keyed by type. */
const VARIANT_FIXTURES: Record<string, Event> = {
  WorkflowStarted: workflowStarted(1),
  WorkflowCompleted: workflowCompleted(1),
  WorkflowFailed: {
    type: 'WorkflowFailed',
    data: { envelope: envelope(1), error: { message: 'boom', details: null } },
  },
  WorkflowCancelled: {
    type: 'WorkflowCancelled',
    data: { envelope: envelope(1), reason: 'operator' },
  },
  WorkflowTimedOut: {
    type: 'WorkflowTimedOut',
    data: { envelope: envelope(1), timeout: 'execution' },
  },
  WorkflowContinuedAsNew: {
    type: 'WorkflowContinuedAsNew',
    data: {
      envelope: envelope(1),
      input: jsonPayload,
      workflow_type: 'checkout-v2',
      parent_run_id: '00000000-0000-0000-0000-0000000000a1',
    },
  },
  WorkflowReopened: {
    type: 'WorkflowReopened',
    data: { envelope: envelope(1), run_id: '00000000-0000-0000-0000-0000000000a1', reopened: [7] },
  },
  SearchAttributesUpdated: {
    type: 'SearchAttributesUpdated',
    data: {
      envelope: envelope(1),
      workflow_id: workflowId,
      attributes: { customer: { type: 'String', data: 'acme' } },
    },
  },
  ActivityScheduled: activityScheduled(1),
  ActivityStarted: activityStarted(1),
  ActivityCompleted: activityCompleted(1),
  ActivityFailed: activityFailed(1, 1),
  ActivityCancelled: {
    type: 'ActivityCancelled',
    data: { envelope: envelope(1), activity_id: 7, attempt: 1 },
  },
  TimerStarted: {
    type: 'TimerStarted',
    data: { envelope: envelope(1), timer_id: { Named: 'retry' }, fire_at: '2026-06-05T20:01:00Z' },
  },
  TimerFired: {
    type: 'TimerFired',
    data: { envelope: envelope(1), timer_id: { Named: 'retry' } },
  },
  TimerCancelled: {
    type: 'TimerCancelled',
    data: { envelope: envelope(1), timer_id: { Named: 'retry' } },
  },
  WithTimeoutCompleted: {
    type: 'WithTimeoutCompleted',
    data: {
      envelope: envelope(1),
      timer_id: { Named: 'bound' },
      outcome: 'OperationCompleted',
      result: jsonPayload,
    },
  },
  SignalReceived: signalReceived(1),
  SignalSent: {
    type: 'SignalSent',
    data: {
      envelope: envelope(1),
      target_workflow_id: 'peer-1',
      name: 'notify',
      payload: jsonPayload,
    },
  },
  ChildWorkflowStarted: childStarted(1),
  ChildWorkflowCompleted: childCompleted(1),
  ChildWorkflowFailed: {
    type: 'ChildWorkflowFailed',
    data: {
      envelope: envelope(1),
      child_workflow_id: 'child-1',
      error: { message: 'child boom', details: null },
    },
  },
  ChildWorkflowCancelled: {
    type: 'ChildWorkflowCancelled',
    data: { envelope: envelope(1), child_workflow_id: 'child-1' },
  },
  ScheduleCreated: {
    type: 'ScheduleCreated',
    data: { envelope: envelope(1), schedule_id: 'sched-1', config: scheduleConfig },
  },
  ScheduleUpdated: {
    type: 'ScheduleUpdated',
    data: { envelope: envelope(1), schedule_id: 'sched-1', config: scheduleConfig },
  },
  SchedulePaused: {
    type: 'SchedulePaused',
    data: { envelope: envelope(1), schedule_id: 'sched-1' },
  },
  ScheduleResumed: {
    type: 'ScheduleResumed',
    data: { envelope: envelope(1), schedule_id: 'sched-1' },
  },
  ScheduleDeleted: {
    type: 'ScheduleDeleted',
    data: { envelope: envelope(1), schedule_id: 'sched-1' },
  },
  ScheduleTriggered: {
    type: 'ScheduleTriggered',
    data: {
      envelope: envelope(1),
      schedule_id: 'sched-1',
      workflow_id: 'run-1',
      run_id: '00000000-0000-0000-0000-0000000000a2',
    },
  },
};

const LANE_VARIANTS: Record<string, string> = {
  WorkflowStarted: 'lifecycle',
  WorkflowCompleted: 'lifecycle',
  WorkflowFailed: 'lifecycle',
  WorkflowCancelled: 'lifecycle',
  WorkflowTimedOut: 'lifecycle',
  ActivityScheduled: 'activity',
  ActivityStarted: 'activity',
  ActivityCompleted: 'activity',
  ActivityFailed: 'activity',
  ActivityCancelled: 'activity',
  TimerStarted: 'timer',
  TimerFired: 'timer',
  TimerCancelled: 'timer',
  WithTimeoutCompleted: 'timer',
  SignalReceived: 'signal',
  SignalSent: 'signal',
  ChildWorkflowStarted: 'child',
  ChildWorkflowCompleted: 'child',
  ChildWorkflowFailed: 'child',
  ChildWorkflowCancelled: 'child',
};

test('every one of the 29 variants is covered by a fixture', () => {
  for (const eventType of KNOWN_EVENT_TYPES) {
    expect(VARIANT_FIXTURES[eventType]).toBeDefined();
  }
});

test('each of the 29 variants projects to a real typed row (no silent drop)', () => {
  for (const eventType of KNOWN_EVENT_TYPES) {
    const event = VARIANT_FIXTURES[eventType];

    if (event === undefined) {
      throw new Error(`missing fixture for ${eventType}`);
    }

    const [entry] = projectTimeline([event]);

    if (entry === undefined) {
      throw new Error(`no timeline entry projected for ${eventType}`);
    }

    expect(entry.summary.length).toBeGreaterThan(0);

    const expectedLane = LANE_VARIANTS[eventType];

    if (expectedLane) {
      expect(entry.kind).toBe(expectedLane as TimelineEntry['kind']);
    } else {
      // marker + schedule variants render as a typed generic row carrying their
      // classifier family + sub-kind — never an unlabelled fallback.
      expect(entry.kind).toBe('generic');

      if (entry.kind === 'generic') {
        expect(entry.family).not.toBeNull();
        expect(entry.subKind).toBe(eventType);
      }
    }
  }
});

test('SignalSent carries direction + target; WithTimeoutCompleted closes the timer lane', () => {
  const sent = projectTimeline([VARIANT_FIXTURES.SignalSent as Event])[0];

  if (sent?.kind !== 'signal') {
    throw new Error('expected a signal entry');
  }

  expect(sent.direction).toBe('sent');
  expect(sent.targetWorkflowId).toBe('peer-1');

  const timedOut = projectTimeline([
    {
      type: 'WithTimeoutCompleted',
      data: {
        envelope: envelope(1),
        timer_id: { Named: 'bound' },
        outcome: 'TimedOut',
        result: null,
      },
    },
  ])[0];

  if (timedOut?.kind !== 'timer') {
    throw new Error('expected a timer entry');
  }

  expect(timedOut.status).toBe('timed-out');
});

test('a genuinely-unknown future variant renders a generic row, never throwing', () => {
  // Simulate a server that emitted a variant the wire types do not yet know.
  const future = {
    type: 'SomeFutureVariant',
    data: { envelope: envelope(1) },
  } as unknown as Event;

  const entry = projectTimeline([future])[0];
  expect(entry?.kind).toBe('generic');

  if (entry?.kind === 'generic') {
    expect(entry.family).toBeNull();
    expect(entry.subKind).toBeNull();
    expect(entry.summary.length).toBeGreaterThan(0);
  }
});

test('a WorkflowIntent timer cancel stands as a real cancel', () => {
  const entry = projectTimeline([
    {
      type: 'TimerStarted',
      data: {
        envelope: envelope(1),
        timer_id: { Named: 'retry' },
        fire_at: '2026-07-04T01:00:00Z',
      },
    },
    {
      type: 'TimerCancelled',
      data: { envelope: envelope(2), timer_id: { Named: 'retry' }, cause: 'WorkflowIntent' },
    },
  ])[0];

  if (entry?.kind !== 'timer') {
    throw new Error('expected a timer entry');
  }
  expect(entry.status).toBe('cancelled');
  expect(entry.cancelCause).toBe('WorkflowIntent');
  expect(entry.cancelled).not.toBeNull();
  expect(entry.rearmed).toBe(false);
});

test('a missing timer cancel cause defaults to WorkflowIntent (serde-default parity)', () => {
  const entry = projectTimeline([
    {
      type: 'TimerStarted',
      data: {
        envelope: envelope(1),
        timer_id: { Named: 'retry' },
        fire_at: '2026-07-04T01:00:00Z',
      },
    },
    { type: 'TimerCancelled', data: { envelope: envelope(2), timer_id: { Named: 'retry' } } },
  ])[0];

  if (entry?.kind !== 'timer') {
    throw new Error('expected a timer entry');
  }
  expect(entry.status).toBe('cancelled');
  expect(entry.cancelCause).toBe('WorkflowIntent');
});

test('a CancelTeardown cancel is muted, and a reopen re-arms the SAME timer lane', () => {
  // Teardown → WorkflowReopened → fresh TimerStarted (same id, original fire_at)
  // → TimerFired. One timer id must remain ONE lane, and the folded teardown must
  // not leave the lane reading as cancelled.
  const timeline = projectTimeline([
    {
      type: 'TimerStarted',
      data: { envelope: envelope(1), timer_id: { Named: 'sla' }, fire_at: '2026-07-04T02:00:00Z' },
    },
    {
      type: 'TimerCancelled',
      data: { envelope: envelope(2), timer_id: { Named: 'sla' }, cause: 'CancelTeardown' },
    },
    {
      type: 'WorkflowReopened',
      data: { envelope: envelope(3), run_id: '00000000-0000-0000-0000-0000000000a1', reopened: [] },
    },
    {
      type: 'TimerStarted',
      data: { envelope: envelope(4), timer_id: { Named: 'sla' }, fire_at: '2026-07-04T02:00:00Z' },
    },
    { type: 'TimerFired', data: { envelope: envelope(5), timer_id: { Named: 'sla' } } },
  ]);

  const timerLanes = timeline.filter((entry) => entry.kind === 'timer');
  expect(timerLanes).toHaveLength(1);

  const timer = timerLanes[0];
  if (timer?.kind !== 'timer') {
    throw new Error('expected a timer entry');
  }
  expect(timer.status).toBe('fired');
  expect(timer.cancelled).toBeNull();
  expect(timer.cancelCause).toBeNull();
  expect(timer.rearmed).toBe(true);
});

test('a standalone CancelTeardown (never re-armed) folds to the pre-cancel state, not cancelled', () => {
  const entry = projectTimeline([
    {
      type: 'TimerStarted',
      data: { envelope: envelope(1), timer_id: { Named: 'sla' }, fire_at: '2026-07-04T02:00:00Z' },
    },
    {
      type: 'TimerCancelled',
      data: { envelope: envelope(2), timer_id: { Named: 'sla' }, cause: 'CancelTeardown' },
    },
  ])[0];

  if (entry?.kind !== 'timer') {
    throw new Error('expected a timer entry');
  }
  expect(entry.status).toBe('started');
  expect(entry.cancelled).toBeNull();
  expect(entry.cancelCause).toBe('CancelTeardown');
});

test('payload decode failure falls back to the raw envelope, no throw', () => {
  // Bytes that are not valid JSON despite a Json content-type.
  const badJson: Payload = { content_type: 'Json', bytes: [123, 34] };
  expect(payloadSummary(badJson)).toContain('{');

  const event: Event = {
    type: 'WorkflowCompleted',
    data: { envelope: envelope(1), result: badJson },
  };
  const entry = projectTimeline([event])[0];
  expect(entry?.kind).toBe('lifecycle');
  expect(entry?.summary).toContain('Workflow completed');
});
