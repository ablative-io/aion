import { expect, test } from 'bun:test';

import type { Event, Payload } from '@/types';

import { projectTimeline } from '../lib/timeline';
import { layoutSwimlane } from './laneLayout';
import { cutsAtTimestamp, prefixUpTo } from './scrub';
import {
  buildAxisLayout,
  clusterPositionedBars,
  eventKey,
  fractionForTimestamp,
  MIN_BAR_WIDTH,
  MIN_STEPPED_RANK_WIDTH,
  orderedVisibleEvents,
  positionBar,
  timeBounds,
  type VisibleWorkflow,
} from './timeLayout';

const PAYLOAD: Payload = { content_type: 'Json', bytes: [123, 125] };
const BASE = Date.parse('2026-07-15T00:00:00Z');

function envelope(seq: number, workflowId: string, second: number) {
  return {
    seq,
    recorded_at: new Date(BASE + second * 1000).toISOString(),
    workflow_id: workflowId,
  };
}

function activityScheduled(seq: number, workflowId: string, second: number): Event {
  return {
    type: 'ActivityScheduled',
    data: {
      envelope: envelope(seq, workflowId, second),
      activity_id: 1,
      activity_type: 'charge',
      input: PAYLOAD,
      task_queue: 'default',
      node: null,
    },
  };
}

function activityCompleted(seq: number, workflowId: string, second: number): Event {
  return {
    type: 'ActivityCompleted',
    data: {
      envelope: envelope(seq, workflowId, second),
      activity_id: 1,
      attempt: 1,
      result: PAYLOAD,
    },
  };
}

function signalReceived(seq: number, workflowId: string, second: number): Event {
  return {
    type: 'SignalReceived',
    data: { envelope: envelope(seq, workflowId, second), name: `signal-${seq}`, payload: PAYLOAD },
  };
}

function workflow(
  workflowId: string,
  events: readonly Event[],
  isRunning = false
): VisibleWorkflow {
  return { workflowId, entries: projectTimeline(events), isRunning };
}

test('time axis maps recorded_at spans fractionally across the shared parent/child extent', () => {
  const parent = workflow('parent', [
    activityScheduled(1, 'parent', 0),
    activityCompleted(2, 'parent', 5),
  ]);
  const child = workflow('child', [signalReceived(1, 'child', 10)]);
  const axis = buildAxisLayout([parent, child], 'time', 100, BASE + 10_000);
  if (axis === null) {
    throw new Error('expected a time axis');
  }
  const bar = layoutSwimlane(parent.entries).lanes.find((lane) => lane.kind === 'activity')
    ?.bars[0];
  if (bar === undefined) {
    throw new Error('expected the parent activity bar');
  }

  const positioned = positionBar('parent', bar, axis);
  expect(positioned.left).toBe(0);
  expect(positioned.width).toBe(50);
  expect(positioned.marker).toBe(false);
  expect(fractionForTimestamp(BASE + 5_000, axis.bounds)).toBe(0.5);
});

test('sub-threshold bars get the minimum marker floor and overlapping neighbors cluster', () => {
  const visible = workflow('parent', [
    signalReceived(1, 'parent', 2),
    signalReceived(2, 'parent', 2),
    signalReceived(3, 'parent', 2),
  ]);
  const axis = buildAxisLayout([visible], 'time', 100, BASE + 2_000);
  const signalLane = layoutSwimlane(visible.entries).lanes.find((lane) => lane.kind === 'signal');
  if (axis === null || signalLane === undefined) {
    throw new Error('expected signal markers');
  }

  const positioned = signalLane.bars.map((bar) => positionBar('parent', bar, axis));
  expect(positioned.every((bar) => bar.marker && bar.width >= MIN_BAR_WIDTH)).toBe(true);
  const items = clusterPositionedBars(positioned);
  expect(items).toHaveLength(1);
  expect(items[0]?.kind).toBe('cluster');
  if (items[0]?.kind === 'cluster') {
    expect(items[0].members).toHaveLength(3);
  }
});

test('running shared extent uses now while a terminal extent stops at its last event', () => {
  const running = workflow('running', [signalReceived(1, 'running', 1)], true);
  const terminal = workflow('terminal', [signalReceived(1, 'terminal', 5)]);

  expect(timeBounds([running, terminal], BASE + 20_000)).toEqual({
    t0: BASE + 1_000,
    tEnd: BASE + 20_000,
  });
  expect(timeBounds([{ ...running, isRunning: false }, terminal], BASE + 20_000)).toEqual({
    t0: BASE + 1_000,
    tEnd: BASE + 5_000,
  });
});

test('stepped mode orders the union by recorded_at then seq across workflows', () => {
  const parent = workflow('parent', [
    signalReceived(10, 'parent', 5),
    signalReceived(2, 'parent', 3),
  ]);
  const child = workflow('child', [signalReceived(1, 'child', 1)]);
  const ordered = orderedVisibleEvents([parent, child]);
  expect(ordered.map((event) => [event.workflowId, event.sequence])).toEqual([
    ['child', 1],
    ['parent', 2],
    ['parent', 10],
  ]);

  const stepped = buildAxisLayout([parent, child], 'stepped', 40, BASE + 5_000);
  if (stepped === null) {
    throw new Error('expected a stepped axis');
  }
  expect(stepped.rankByEvent.get(eventKey('child', 1))).toBe(0);
  expect(stepped.rankByEvent.get(eventKey('parent', 10))).toBe(2);
  expect(stepped.rankWidth).toBe(MIN_STEPPED_RANK_WIDTH);
  expect(stepped.trackWidth).toBe(3 * MIN_STEPPED_RANK_WIDTH);

  const time = buildAxisLayout([parent, child], 'time', 40, BASE + 5_000);
  expect(time?.trackWidth).toBe(40);
});

/** The straddle fixture: one activity spanning 0s → 10s, scrubbed mid-span. */
function straddleFixture(cursorMs: number) {
  const parent = workflow('parent', [
    activityScheduled(1, 'parent', 0),
    activityCompleted(2, 'parent', 10),
  ]);
  const axis = buildAxisLayout([parent], 'time', 100, BASE + 10_000);
  const fullEntry = parent.entries.find((entry) => entry.kind === 'activity');
  const cut = cutsAtTimestamp([parent], cursorMs).get('parent');
  const bar = layoutSwimlane(prefixUpTo(parent.entries, cut ?? null)).lanes.find(
    (lane) => lane.kind === 'activity'
  )?.bars[0];
  if (axis === null || fullEntry === undefined || bar === undefined) {
    throw new Error('expected the straddling activity bar');
  }
  return { parent, axis, fullEntry, bar };
}

test('a span straddling the time cursor stays visible, clipped to the cursor', () => {
  const { axis, fullEntry, bar } = straddleFixture(BASE + 5_000);

  // Without the clip, the truncated bar collapses to a marker at its start —
  // the pop the operator reported.
  const unclipped = positionBar('parent', bar, axis);
  expect(unclipped.marker).toBe(true);

  // With the clip it renders its true start with a width that flows to the cursor.
  const atHalf = positionBar('parent', bar, axis, MIN_BAR_WIDTH, {
    cursor: BASE + 5_000,
    fullEntry,
  });
  expect(atHalf.left).toBe(0);
  expect(atHalf.width).toBe(50);
  expect(atHalf.marker).toBe(false);

  const nearEnd = positionBar('parent', bar, axis, MIN_BAR_WIDTH, {
    cursor: BASE + 8_000,
    fullEntry,
  });
  expect(nearEnd.width).toBe(80);

  // A cursor barely past the start keeps the bar visible on the marker floor.
  const nearStart = positionBar('parent', bar, axis, MIN_BAR_WIDTH, {
    cursor: BASE + 100,
    fullEntry,
  });
  expect(nearStart.marker).toBe(true);
  expect(nearStart.width).toBeGreaterThanOrEqual(MIN_BAR_WIDTH);
});

test('a span entirely before the cursor stays fully drawn and fit-to-view is unaffected', () => {
  const { parent, axis, fullEntry } = straddleFixture(BASE + 10_000);
  const fullBar = layoutSwimlane(prefixUpTo(parent.entries, 2)).lanes.find(
    (lane) => lane.kind === 'activity'
  )?.bars[0];
  if (fullBar === undefined) {
    throw new Error('expected the full activity bar');
  }

  const clipped = positionBar('parent', fullBar, axis, MIN_BAR_WIDTH, {
    cursor: BASE + 10_000,
    fullEntry,
  });
  const live = positionBar('parent', fullBar, axis);
  expect(clipped).toEqual(live);
  expect(clipped.width).toBe(100);

  // The shared axis is built from the untruncated workflows: scrubbing never
  // rescales fit-to-view.
  expect(axis.trackWidth).toBe(100);
  expect(axis.bounds).toEqual({ t0: BASE, tEnd: BASE + 10_000 });
});

test('a straddling event disappears only once the cursor precedes its start', () => {
  const parent = workflow('parent', [
    activityScheduled(1, 'parent', 2),
    activityCompleted(2, 'parent', 10),
  ]);

  // Before the span starts: no cut for the workflow, so nothing survives.
  expect(cutsAtTimestamp([parent], BASE + 1_000).get('parent')).toBeNull();

  // From the first event onward the entry survives the prefix cut.
  const cut = cutsAtTimestamp([parent], BASE + 2_000).get('parent');
  expect(cut).toBe(1);
  expect(prefixUpTo(parent.entries, cut ?? null)).toHaveLength(1);
});

test('stepped clipping extends a straddling bar to the cursor rank', () => {
  const parent = workflow('parent', [
    activityScheduled(1, 'parent', 0),
    signalReceived(2, 'parent', 5),
    activityCompleted(3, 'parent', 10),
  ]);
  const axis = buildAxisLayout([parent], 'stepped', 0, BASE + 10_000);
  const fullEntry = parent.entries.find((entry) => entry.kind === 'activity');
  const bar = layoutSwimlane(prefixUpTo(parent.entries, 1)).lanes.find(
    (lane) => lane.kind === 'activity'
  )?.bars[0];
  if (axis === null || fullEntry === undefined || bar === undefined) {
    throw new Error('expected the stepped straddling bar');
  }

  // Cursor at rank 1 (the signal): the activity spans ranks 0..2 but renders 0..1.
  const clipped = positionBar('parent', bar, axis, MIN_BAR_WIDTH, { cursor: 1, fullEntry });
  expect(clipped.left).toBe(0);
  expect(clipped.width).toBe(2 * MIN_STEPPED_RANK_WIDTH - 4);
  expect(clipped.marker).toBe(false);

  // Without the clip the truncated single-event bar is a marker (the pop).
  expect(positionBar('parent', bar, axis).marker).toBe(true);
});
