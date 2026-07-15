import { expect, test } from 'bun:test';

import type { Event, Payload } from '@/types';

import { projectTimeline } from '../lib/timeline';
import { layoutSwimlane } from './laneLayout';
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
