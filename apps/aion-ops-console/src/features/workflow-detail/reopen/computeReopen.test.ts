import { describe, expect, test } from 'bun:test';

import type { ActivityErrorKind, Event } from '@/types';

import { computeReopen } from './computeReopen';

const WORKFLOW_ID = '00000000-0000-0000-0000-000000000001';

function envelope(seq: number) {
  return {
    seq,
    recorded_at: `2026-06-29T00:00:${String(seq).padStart(2, '0')}Z`,
    workflow_id: WORKFLOW_ID,
  };
}

function scheduled(seq: number, activityId: number, activityType: string): Event {
  return {
    type: 'ActivityScheduled',
    data: {
      envelope: envelope(seq),
      activity_id: activityId,
      activity_type: activityType,
      input: { content_type: 'Json', bytes: [] },
      task_queue: 'default',
      node: null,
    },
  };
}

function completed(seq: number, activityId: number): Event {
  return {
    type: 'ActivityCompleted',
    data: {
      envelope: envelope(seq),
      activity_id: activityId,
      result: { content_type: 'Json', bytes: [] },
    },
  };
}

function failed(seq: number, activityId: number, attempt: number, kind: ActivityErrorKind): Event {
  return {
    type: 'ActivityFailed',
    data: {
      envelope: envelope(seq),
      activity_id: activityId,
      error: { kind, message: `boom-${activityId}`, details: null },
      attempt,
    },
  };
}

describe('computeReopen', () => {
  test('completed activity is reused (preserved), not reopened', () => {
    const plan = computeReopen([scheduled(1, 10, 'charge'), completed(2, 10)]);

    expect(plan.reopenable).toBe(false);
    expect(plan.reopened).toEqual([]);
    expect(plan.cursorResetSeq).toBeNull();
    const row = plan.rows.find((entry) => entry.activityId === 10);
    expect(row?.disposition).toBe('reused');
  });

  test('terminal-failed activity with no later success re-dispatches and is superseded', () => {
    const plan = computeReopen([scheduled(1, 20, 'ship'), failed(2, 20, 1, 'Terminal')]);

    expect(plan.reopenable).toBe(true);
    expect(plan.reopened).toEqual([20]);
    expect(plan.cursorResetSeq).toBe(2);

    const dispositions = plan.rows
      .filter((row) => row.activityId === 20)
      .map((row) => row.disposition)
      .sort();
    expect(dispositions).toEqual(['redispatch', 'superseded']);

    const superseded = plan.rows.find(
      (row) => row.activityId === 20 && row.disposition === 'superseded'
    );
    expect(superseded?.failureMessage).toBe('boom-20');
  });

  test('a later success supersedes an earlier failure: reused, not reopened', () => {
    const plan = computeReopen([
      scheduled(1, 30, 'retryable-then-ok'),
      failed(2, 30, 1, 'Terminal'),
      completed(3, 30),
    ]);

    expect(plan.reopened).toEqual([]);
    expect(plan.rows.find((row) => row.activityId === 30)?.disposition).toBe('reused');
  });

  test('retryable-only failure (no terminal, no success) is left to replay, not reopened', () => {
    const plan = computeReopen([scheduled(1, 40, 'pending'), failed(2, 40, 1, 'Retryable')]);

    expect(plan.reopened).toEqual([]);
    expect(plan.rows.some((row) => row.activityId === 40)).toBe(false);
  });

  test('cursor reset is the earliest superseded failure across multiple reopened activities', () => {
    const plan = computeReopen([
      scheduled(1, 50, 'a'),
      scheduled(2, 60, 'b'),
      failed(4, 60, 1, 'Terminal'),
      failed(3, 50, 1, 'Terminal'),
    ]);

    expect(plan.reopened.sort()).toEqual([50, 60]);
    expect(plan.cursorResetSeq).toBe(3);
  });

  test('attempt count reflects the highest observed one-based attempt', () => {
    const plan = computeReopen([
      scheduled(1, 70, 'flaky'),
      failed(2, 70, 1, 'Retryable'),
      failed(3, 70, 2, 'Terminal'),
    ]);

    const redispatch = plan.rows.find(
      (row) => row.activityId === 70 && row.disposition === 'redispatch'
    );
    expect(redispatch?.attempts).toBe(2);
  });

  test('empty history yields an empty, non-reopenable plan', () => {
    const plan = computeReopen([]);
    expect(plan.reopenable).toBe(false);
    expect(plan.rows).toEqual([]);
    expect(plan.cursorResetSeq).toBeNull();
  });
});
