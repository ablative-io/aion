import { expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import type { Event, Payload, WorkflowId } from '@/types';

import { projectTimeline } from '../lib/timeline';
import { layoutSwimlane } from './laneLayout';
import { Scrubber } from './Scrubber';
import { Swimlane } from './Swimlane';
import {
  cutsAtGlobalRank,
  cutsAtTimestamp,
  prefixUpTo,
  scrubSequences,
  snapGlobalRank,
} from './scrub';
import { buildAxisLayout, type VisibleWorkflow } from './timeLayout';

const WORKFLOW_ID = 'workflow-1';
const BASE = Date.parse('2026-07-15T00:00:00Z');
const PAYLOAD: Payload = { content_type: 'Json', bytes: [123, 125] };

function envelope(seq: number, workflowId = WORKFLOW_ID, second = seq) {
  return {
    seq,
    recorded_at: new Date(BASE + second * 1000).toISOString(),
    workflow_id: workflowId,
  };
}

function workflowStarted(seq: number): Event {
  return {
    type: 'WorkflowStarted',
    data: {
      envelope: envelope(seq),
      workflow_type: 'checkout',
      input: PAYLOAD,
      run_id: '00000000-0000-0000-0000-0000000000a1',
      parent_run_id: null,
      package_version: '1.0.0',
    },
  };
}

function activityScheduled(seq: number): Event {
  return {
    type: 'ActivityScheduled',
    data: {
      envelope: envelope(seq),
      activity_id: 10,
      activity_type: 'charge',
      input: PAYLOAD,
      task_queue: 'default',
      node: null,
    },
  };
}

function activityStarted(seq: number): Event {
  return {
    type: 'ActivityStarted',
    data: { envelope: envelope(seq), activity_id: 10, attempt: 1 },
  };
}

function activityCompleted(seq: number): Event {
  return {
    type: 'ActivityCompleted',
    data: { envelope: envelope(seq), activity_id: 10, attempt: 1, result: PAYLOAD },
  };
}

function signalReceived(seq: number, workflowId: string, second: number): Event {
  return {
    type: 'SignalReceived',
    data: { envelope: envelope(seq, workflowId, second), name: 'child-signal', payload: PAYLOAD },
  };
}

function fullRun() {
  return projectTimeline([
    workflowStarted(1),
    activityScheduled(2),
    activityStarted(3),
    activityCompleted(4),
  ]);
}

function visibleWorkflows(): VisibleWorkflow[] {
  return [
    { workflowId: WORKFLOW_ID, entries: fullRun(), isRunning: false },
    {
      workflowId: 'child-1',
      entries: projectTimeline([signalReceived(50, 'child-1', 2)]),
      isRunning: false,
    },
  ];
}

test('prefix reconstruction remains sequence-exact and re-derives status', () => {
  expect(scrubSequences(fullRun())).toEqual([1, 2, 3, 4]);
  expect(prefixUpTo(fullRun(), 1).some((entry) => entry.kind === 'activity')).toBe(false);

  const entry = prefixUpTo(fullRun(), 3).find((candidate) => candidate.kind === 'activity');
  if (entry === undefined || entry.kind !== 'activity') {
    throw new Error('expected activity at the cut');
  }
  expect(entry.status).toBe('started');
  expect(entry.completed).toBeNull();
  expect(
    layoutSwimlane(prefixUpTo(fullRun(), 1)).lanes.some((lane) => lane.kind === 'activity')
  ).toBe(false);
});

test('continuous shared time maps to an independent cut for every visible workflow', () => {
  const cuts = cutsAtTimestamp(visibleWorkflows(), BASE + 2_500);
  expect(cuts.get(WORKFLOW_ID)).toBe(2);
  expect(cuts.get('child-1')).toBe(50);

  const plateau = cutsAtTimestamp(visibleWorkflows(), BASE + 2_900);
  expect(plateau).toEqual(cuts);
});

test('stepped scrub snaps and includes the exact global-order prefix', () => {
  const workflows = visibleWorkflows();
  // At the shared second, seq 2 precedes child seq 50 by the documented tie-break.
  const cuts = cutsAtGlobalRank(workflows, 2);
  expect(cuts.get(WORKFLOW_ID)).toBe(2);
  expect(cuts.get('child-1')).toBe(50);
  expect(snapGlobalRank(1.6, 4)).toBe(2);
  expect(snapGlobalRank(99, 4)).toBe(3);
});

test('scrubber renders continuous time and stepped global-rank readouts', () => {
  const workflows = visibleWorkflows();
  const timeAxis = buildAxisLayout(workflows, 'time', 500, BASE + 4_000);
  const timeMarkup = renderToStaticMarkup(
    <Scrubber axis={timeAxis} mode="time" onScrub={() => {}} scrubPosition={BASE + 2_500} />
  );
  expect(timeMarkup).toContain('Scrub to recorded time');
  expect(timeMarkup).toContain('2026-07-15T00:00:02.500Z');

  const steppedAxis = buildAxisLayout(workflows, 'stepped', 500, BASE + 4_000);
  const steppedMarkup = renderToStaticMarkup(
    <Scrubber axis={steppedAxis} mode="stepped" onScrub={() => {}} scrubPosition={2} />
  );
  expect(steppedMarkup).toContain('child-1 seq 50');
});

test('Swimlane applies the shared time cut to its workflow prefix', () => {
  const markup = renderToStaticMarkup(
    <Swimlane
      entries={fullRun()}
      isRunning={false}
      nowMs={BASE + 4_000}
      onScrubChange={() => {}}
      onSelect={() => {}}
      scrubPosition={BASE + 1_000}
      selectedSequence={null}
      workflowId={WORKFLOW_ID as WorkflowId}
    />
  );
  expect(markup).toContain('Workflow swimlane');
  expect(markup).not.toContain('charge');
});

test('empty axis yields no scrubber', () => {
  const markup = renderToStaticMarkup(
    <Scrubber axis={null} mode="time" onScrub={() => {}} scrubPosition={null} />
  );
  expect(markup).toBe('');
});
