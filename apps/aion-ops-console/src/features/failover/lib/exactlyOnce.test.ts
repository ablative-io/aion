import { describe, expect, test } from 'bun:test';

import type { Event } from '@/types';

import { completedCount, hasOverCount, isActivityCompleted, tallyExactlyOnce } from './exactlyOnce';

const WORKFLOW_ID = '00000000-0000-0000-0000-000000000001';

function completed(seq: number, activityId: number): Event {
  return {
    type: 'ActivityCompleted',
    data: {
      envelope: {
        seq,
        recorded_at: '2026-06-29T00:00:00Z',
        workflow_id: WORKFLOW_ID,
      },
      activity_id: activityId,
      result: { content_type: 'Json', bytes: [110, 117, 108, 108] },
    },
  } as Event;
}

function started(seq: number): Event {
  return {
    type: 'WorkflowStarted',
    data: {
      envelope: { seq, recorded_at: '2026-06-29T00:00:00Z', workflow_id: WORKFLOW_ID },
      workflow_type: 'collect_four',
      input: { content_type: 'Json', bytes: [123, 125] },
      run_id: '00000000-0000-0000-0000-0000000000a1',
      parent_run_id: null,
      package_version: '1.0.0',
    },
  } as Event;
}

describe('isActivityCompleted', () => {
  test('discriminates the ActivityCompleted variant', () => {
    expect(isActivityCompleted(completed(2, 0))).toBe(true);
    expect(isActivityCompleted(started(1))).toBe(false);
  });
});

describe('tallyExactlyOnce — empty and partial inputs', () => {
  test('empty input yields a zero tally', () => {
    const tally = tallyExactlyOnce([]);
    expect(tally.count).toBe(0);
    expect(tally.distinctOrdinals).toBe(0);
    expect(tally.countedSequences).toEqual([]);
    expect(tally.countedOrdinals).toEqual([]);
  });

  test('ignores non-completion events entirely', () => {
    const tally = tallyExactlyOnce([started(1), started(5)]);
    expect(tally.count).toBe(0);
  });

  test('counts a partial set (work still in progress)', () => {
    const tally = tallyExactlyOnce([started(1), completed(2, 0), completed(3, 1)]);
    expect(tally.count).toBe(2);
    expect(tally.distinctOrdinals).toBe(2);
    expect(tally.countedOrdinals).toEqual([0, 1]);
  });
});

describe('tallyExactlyOnce — dedup by seq', () => {
  test('the same completion delivered twice counts once', () => {
    const tally = tallyExactlyOnce([completed(2, 0), completed(2, 0)]);
    expect(tally.count).toBe(1);
  });

  test('distinct seqs for distinct activities all count', () => {
    const events = [completed(2, 0), completed(3, 1), completed(4, 2), completed(5, 3)];
    const tally = tallyExactlyOnce(events);
    expect(tally.count).toBe(4);
    expect(tally.countedSequences).toEqual([2, 3, 4, 5]);
    expect(tally.countedOrdinals).toEqual([0, 1, 2, 3]);
  });

  test('out-of-order delivery does not affect the count', () => {
    const tally = tallyExactlyOnce([completed(5, 3), completed(2, 0), completed(4, 2)]);
    expect(tally.count).toBe(3);
    expect(tally.countedSequences).toEqual([2, 4, 5]);
  });
});

describe('the reconnect scenario — overlapping history + live, NO double-count', () => {
  // This is the catastrophic-on-stage case: the firehose has no resume cursor, so
  // on reconnect the view back-fills via describe AND keeps the live frames. The
  // two sets OVERLAP. A naive append would show 5/4. seq-dedup must collapse them.

  test('overlapping describe back-fill + live firehose never double-counts (4, never 5)', () => {
    // describe back-fill returned the full history of 4 completions...
    const describeBackfill = [
      started(1),
      completed(2, 0),
      completed(3, 1),
      completed(4, 2),
      completed(5, 3),
    ];
    // ...and the live firehose re-delivered the last two after the reconnect.
    const liveReplay = [completed(4, 2), completed(5, 3)];

    const merged = [...describeBackfill, ...liveReplay];
    const tally = tallyExactlyOnce(merged);

    expect(tally.count).toBe(4);
    expect(tally.count).not.toBe(5);
    expect(completedCount(merged, 4)).toBe(4);
    expect(hasOverCount(merged, 4)).toBe(false);
  });

  test('full overlap (describe and live carry the identical four) still counts 4', () => {
    const four = [completed(2, 0), completed(3, 1), completed(4, 2), completed(5, 3)];
    const tally = tallyExactlyOnce([...four, ...four]);
    expect(tally.count).toBe(4);
  });

  test('live-only fragment before back-fill, then back-fill arrives — still 4', () => {
    const liveFragment = [completed(4, 2), completed(5, 3)];
    const backfill = [completed(2, 0), completed(3, 1), completed(4, 2), completed(5, 3)];
    const tally = tallyExactlyOnce([...liveFragment, ...backfill]);
    expect(tally.count).toBe(4);
    expect(tally.countedOrdinals).toEqual([0, 1, 2, 3]);
  });
});

describe('completedCount + hasOverCount — arity clamp and honest overflow', () => {
  test('clamps the surfaced count to arity', () => {
    const events = [completed(2, 0), completed(3, 1), completed(4, 2), completed(5, 3)];
    expect(completedCount(events, 4)).toBe(4);
    expect(completedCount(events)).toBe(4);
  });

  test('a partial set reports below arity', () => {
    expect(completedCount([completed(2, 0), completed(3, 1)], 4)).toBe(2);
  });

  test('distinct seqs beyond arity are reported as an honest overcount', () => {
    // Five DISTINCT completions for an arity-4 fan-out would be a real contract
    // violation (not a dedup artefact); surface it rather than hide it.
    const five = [
      completed(2, 0),
      completed(3, 1),
      completed(4, 2),
      completed(5, 3),
      completed(6, 4),
    ];
    expect(hasOverCount(five, 4)).toBe(true);
    expect(completedCount(five, 4)).toBe(4);
  });
});
