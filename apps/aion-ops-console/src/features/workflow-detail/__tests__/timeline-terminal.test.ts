import { expect, test } from 'bun:test';

import type { Event } from '@/types';

import {
  mergeEventsBySequence,
  terminalOutcomeForEvents,
  terminalOutcomeForMergedEvents,
} from '../lib/timeline';

const WORKFLOW_ID = 'workflow-terminal-equivalence';

function envelope(seq: number) {
  return {
    seq,
    recorded_at: `2026-06-05T20:00:${String(seq).padStart(2, '0')}Z`,
    workflow_id: WORKFLOW_ID,
  };
}

function workflowFailed(seq: number): Event {
  return {
    type: 'WorkflowFailed',
    data: {
      envelope: envelope(seq),
      error: { message: `failure ${seq}`, details: null },
    },
  };
}

function workflowReopened(seq: number): Event {
  return {
    type: 'WorkflowReopened',
    data: {
      envelope: envelope(seq),
      run_id: '00000000-0000-0000-0000-0000000000a1',
      reopened: [3],
    },
  };
}

test('terminal outcome from pre-merged events matches the public unordered-event contract', () => {
  const events = [workflowFailed(6), workflowReopened(4), workflowFailed(3), workflowFailed(6)];
  const merged = mergeEventsBySequence(events, []);

  expect(terminalOutcomeForMergedEvents(merged)).toBe(terminalOutcomeForEvents(events));
  expect(terminalOutcomeForMergedEvents(merged)).toBe('failed');
});
