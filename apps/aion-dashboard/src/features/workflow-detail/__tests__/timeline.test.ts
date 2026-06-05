import { expect, test } from 'bun:test';

import type { Event, Payload } from '@/types';

import {
  mergeEventsBySequence,
  payloadSummary,
  projectTimeline,
  terminalOutcomeForEvents,
} from '../lib/timeline';

const workflowId = 'workflow-1';
const jsonPayload: Payload = { content_type: 'Json', bytes: [123, 125] };

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
    data: { envelope: envelope(seq), workflow_type: 'checkout', input: jsonPayload },
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
    },
  };
}

function activityStarted(seq: number): Event {
  return { type: 'ActivityStarted', data: { envelope: envelope(seq), activity_id: 7 } };
}

function activityCompleted(seq: number): Event {
  return {
    type: 'ActivityCompleted',
    data: { envelope: envelope(seq), activity_id: 7, result: jsonPayload },
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
    },
  };
}

function childCompleted(seq: number): Event {
  return {
    type: 'ChildWorkflowCompleted',
    data: { envelope: envelope(seq), child_workflow_id: 'child-1', result: jsonPayload },
  };
}
