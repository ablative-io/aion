import { expect, test } from 'bun:test';

import type { Event, Payload } from '@/types';

import { deriveFailureContext } from '../reopen/failureContext';

const jsonPayload: Payload = { content_type: 'Json', bytes: [123, 125] };

function envelope(seq: number) {
  return { seq, recorded_at: `2026-06-05T20:00:0${seq}Z`, workflow_id: 'workflow-1' };
}

function activityScheduled(seq: number, activityId: number, activityType: string): Event {
  return {
    type: 'ActivityScheduled',
    data: {
      envelope: envelope(seq),
      activity_id: activityId,
      activity_type: activityType,
      input: jsonPayload,
      task_queue: 'default',
      node: null,
    },
  };
}

function activityFailedTerminal(seq: number, activityId: number): Event {
  return {
    type: 'ActivityFailed',
    data: {
      envelope: envelope(seq),
      activity_id: activityId,
      attempt: 1,
      error: { kind: 'Terminal', message: 'harness reported failure', details: null },
    },
  };
}

function workflowFailed(seq: number, message: string): Event {
  return {
    type: 'WorkflowFailed',
    data: { envelope: envelope(seq), error: { message, details: null } },
  };
}

function workflowReopened(seq: number): Event {
  return {
    type: 'WorkflowReopened',
    data: {
      envelope: envelope(seq),
      run_id: '00000000-0000-0000-0000-0000000000a1',
      reopened: [1],
    },
  };
}

test('a failed run yields its failed step and reason', () => {
  const context = deriveFailureContext([
    activityScheduled(1, 1, 'developer'),
    activityFailedTerminal(2, 1),
    workflowFailed(3, 'workflow 2 returned error'),
  ]);

  expect(context).toEqual({
    failedStep: 'developer',
    failureReason: 'workflow 2 returned error',
  });
});

test('a reopen SUPERSEDES the failure it re-drives — no failure context while live', () => {
  const context = deriveFailureContext([
    activityScheduled(1, 1, 'developer'),
    activityFailedTerminal(2, 1),
    workflowFailed(3, 'workflow 2 returned error'),
    workflowReopened(4),
  ]);

  expect(context).toBeNull();
});

test('a run that fails ANEW after reopen blames only the post-reopen failure', () => {
  const context = deriveFailureContext([
    activityScheduled(1, 1, 'developer'),
    activityFailedTerminal(2, 1),
    workflowFailed(3, 'workflow 2 returned error'),
    workflowReopened(4),
    activityScheduled(5, 2, 'run_gates'),
    activityFailedTerminal(6, 2),
    workflowFailed(7, 'workflow 3 returned error'),
  ]);

  expect(context).toEqual({
    failedStep: 'run_gates',
    failureReason: 'workflow 3 returned error',
  });
});

test('a running or completed history yields no failure context', () => {
  expect(deriveFailureContext([activityScheduled(1, 1, 'developer')])).toBeNull();
});
