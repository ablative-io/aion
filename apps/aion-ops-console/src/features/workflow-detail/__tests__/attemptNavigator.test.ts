import { expect, test } from 'bun:test';

import type { Event, Payload } from '@/types';

import { deriveAttemptNavigator } from '../lib/attemptNavigator';
import { projectTimeline } from '../lib/timeline';

const jsonPayload: Payload = { content_type: 'Json', bytes: [123, 125] };

function envelope(seq: number) {
  return {
    seq,
    recorded_at: `2026-07-04T00:00:${String(seq).padStart(2, '0')}Z`,
    workflow_id: 'wf-1',
  };
}

function scheduled(seq: number, id: number, type = 'charge-card'): Event {
  return {
    type: 'ActivityScheduled',
    data: {
      envelope: envelope(seq),
      activity_id: id,
      activity_type: type,
      input: jsonPayload,
      task_queue: 'default',
      node: null,
    },
  };
}

function started(seq: number, id: number, attempt: number): Event {
  return { type: 'ActivityStarted', data: { envelope: envelope(seq), activity_id: id, attempt } };
}

function failed(seq: number, id: number, attempt: number): Event {
  return {
    type: 'ActivityFailed',
    data: {
      envelope: envelope(seq),
      activity_id: id,
      attempt,
      error: { kind: 'Retryable', message: `attempt ${attempt} failed`, details: null },
    },
  };
}

function completed(seq: number, id: number, attempt: number): Event {
  return {
    type: 'ActivityCompleted',
    data: { envelope: envelope(seq), activity_id: id, attempt, result: jsonPayload },
  };
}

function cancelled(seq: number, id: number, attempt: number): Event {
  return { type: 'ActivityCancelled', data: { envelope: envelope(seq), activity_id: id, attempt } };
}

test('a terminal failed run with N failed attempts and no success keeps every attempt selectable', () => {
  const timeline = projectTimeline([
    scheduled(1, 7),
    started(2, 7, 1),
    failed(3, 7, 1),
    started(4, 7, 2),
    failed(5, 7, 2),
    started(6, 7, 3),
    failed(7, 7, 3),
    {
      type: 'WorkflowFailed',
      data: { envelope: envelope(8), error: { message: 'gave up', details: null } },
    },
  ]);

  const rows = deriveAttemptNavigator(timeline);

  expect(rows.map((row) => row.key)).toEqual(['7:1', '7:2', '7:3']);
  expect(rows.every((row) => row.status === 'failed')).toBe(true);
  expect(rows.every((row) => row.isLegacy === false)).toBe(true);
});

test('a completed run after retries surfaces the failed attempts plus the successful one', () => {
  const timeline = projectTimeline([
    scheduled(1, 7),
    started(2, 7, 1),
    failed(3, 7, 1),
    started(4, 7, 2),
    completed(5, 7, 2),
  ]);

  const rows = deriveAttemptNavigator(timeline);

  expect(rows).toHaveLength(2);
  expect(rows.map((row) => [row.key, row.status])).toEqual([
    ['7:1', 'failed'],
    ['7:2', 'completed'],
  ]);
});

test('a running run shows the in-flight attempt as started (no terminal yet)', () => {
  const timeline = projectTimeline([scheduled(1, 7), started(2, 7, 1)]);

  const rows = deriveAttemptNavigator(timeline);

  expect(rows).toEqual([
    {
      key: '7:1',
      activityId: '7',
      activityType: 'charge-card',
      attempt: 1,
      isLegacy: false,
      status: 'started',
    },
  ]);
});

test('the legacy attempt:0 sentinel is labelled distinctly and still selectable', () => {
  const timeline = projectTimeline([scheduled(1, 7), started(2, 7, 0), failed(3, 7, 0)]);

  const rows = deriveAttemptNavigator(timeline);

  expect(rows).toHaveLength(1);
  expect(rows[0]?.key).toBe('7:0');
  expect(rows[0]?.isLegacy).toBe(true);
  expect(rows[0]?.status).toBe('failed');
});

test('a cancelled attempt reads as cancelled and rows sort by activity then attempt', () => {
  const timeline = projectTimeline([
    scheduled(1, 9),
    started(2, 9, 1),
    cancelled(3, 9, 1),
    scheduled(4, 2),
    started(5, 2, 1),
    completed(6, 2, 1),
  ]);

  const rows = deriveAttemptNavigator(timeline);

  // Numeric activity ordering: activity 2 before activity 9 despite event order.
  expect(rows.map((row) => row.key)).toEqual(['2:1', '9:1']);
  expect(rows.find((row) => row.key === '9:1')?.status).toBe('cancelled');
});

test('a scheduled-only activity with no started attempt yields no navigator rows', () => {
  const rows = deriveAttemptNavigator(projectTimeline([scheduled(1, 7)]));
  expect(rows).toEqual([]);
});
