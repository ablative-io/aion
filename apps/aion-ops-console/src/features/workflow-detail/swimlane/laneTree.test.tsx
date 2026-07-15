import { expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import type { Event, Payload, WorkflowId } from '@/types';

import { projectTimeline } from '../lib/timeline';
import {
  childNodePath,
  type ChildTimelineState,
  flattenLaneTree,
  isWorkflowCycle,
} from './laneTree';
import { resolveSelectionSurface } from './selectionSurface';
import { selectionForBar, Swimlane } from './Swimlane';

const ROOT = 'root-1';
const CHILD = 'child-1';
const GRANDCHILD = 'grand-1';
const PAYLOAD: Payload = { content_type: 'Json', bytes: [123, 125] };

function envelope(seq: number, workflowId: string, second: number) {
  return {
    seq,
    recorded_at: `2026-07-15T00:00:${String(second).padStart(2, '0')}Z`,
    workflow_id: workflowId,
  };
}

function workflowStarted(seq: number, workflowId: string, second: number): Event {
  return {
    type: 'WorkflowStarted',
    data: {
      envelope: envelope(seq, workflowId, second),
      workflow_type: 'test',
      input: PAYLOAD,
      run_id: '00000000-0000-0000-0000-0000000000a1',
      parent_run_id: null,
      package_version: '1.0.0',
    },
  };
}

function childStarted(
  seq: number,
  workflowId: string,
  childWorkflowId: string,
  second: number
): Event {
  return {
    type: 'ChildWorkflowStarted',
    data: {
      envelope: envelope(seq, workflowId, second),
      child_workflow_id: childWorkflowId,
      workflow_type: 'child',
      input: PAYLOAD,
      package_version: '1.0.0',
    },
  };
}

function activityScheduled(seq: number, workflowId: string, second: number): Event {
  return {
    type: 'ActivityScheduled',
    data: {
      envelope: envelope(seq, workflowId, second),
      activity_id: 7,
      activity_type: 'charge-card',
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
      activity_id: 7,
      attempt: 1,
      result: PAYLOAD,
    },
  };
}

const rootEntries = projectTimeline([childStarted(1, ROOT, CHILD, 1)]);
const childEntries = projectTimeline([
  workflowStarted(1, CHILD, 2),
  activityScheduled(2, CHILD, 3),
  activityCompleted(3, CHILD, 8),
  childStarted(4, CHILD, GRANDCHILD, 9),
]);
const grandchildEntries = projectTimeline([
  workflowStarted(1, GRANDCHILD, 4),
  activityScheduled(2, GRANDCHILD, 5),
]);
const childPath = childNodePath(ROOT, CHILD);
const grandchildPath = childNodePath(childPath, GRANDCHILD);

function readyChildren(): ReadonlyMap<string, ChildTimelineState> {
  return new Map<string, ChildTimelineState>([
    [childPath, { status: 'ready', entries: childEntries, isRunning: false }],
    [grandchildPath, { status: 'ready', entries: grandchildEntries, isRunning: false }],
  ]);
}

test('expanded child and grandchild lanes splice directly below their parent lane with depth', () => {
  const tree = flattenLaneTree(
    { workflowId: ROOT, entries: rootEntries, isRunning: false },
    readyChildren(),
    new Set([childPath, grandchildPath])
  );

  expect(tree.rows[0]?.kind).toBe('lane');
  expect(tree.rows[0]?.depth).toBe(0);
  expect(tree.rows.some((row) => row.depth === 1 && row.workflowId === CHILD)).toBe(true);
  expect(tree.rows.some((row) => row.depth === 2 && row.workflowId === GRANDCHILD)).toBe(true);
  expect(tree.workflows.map((workflow) => workflow.workflowId)).toEqual([ROOT, CHILD, GRANDCHILD]);
});

test('child loading is lazy: only an expanded path requests history and collapse drops rows', () => {
  const collapsed = flattenLaneTree(
    { workflowId: ROOT, entries: rootEntries, isRunning: false },
    new Map(),
    new Set()
  );
  expect(collapsed.requests).toHaveLength(0);

  const expanded = flattenLaneTree(
    { workflowId: ROOT, entries: rootEntries, isRunning: false },
    new Map(),
    new Set([childPath])
  );
  expect(expanded.requests.map((request) => request.path)).toEqual([childPath]);
  expect(expanded.rows.some((row) => row.kind === 'notice' && row.notice === 'loading')).toBe(true);

  const recollapsed = flattenLaneTree(
    { workflowId: ROOT, entries: rootEntries, isRunning: false },
    readyChildren(),
    new Set([childPath, grandchildPath]),
    new Set(),
    new Set([childPath])
  );
  expect(recollapsed.rows.every((row) => row.depth === 0)).toBe(true);
  expect(recollapsed.requests).toHaveLength(0);
});

test('late expansion consumes the complete durable child history', () => {
  const tree = flattenLaneTree(
    { workflowId: ROOT, entries: rootEntries, isRunning: false },
    readyChildren(),
    new Set([childPath])
  );
  const activityRow = tree.rows.find(
    (row) => row.kind === 'lane' && row.workflowId === CHILD && row.lane.kind === 'activity'
  );
  if (activityRow === undefined || activityRow.kind !== 'lane') {
    throw new Error('expected a child activity row');
  }

  expect(activityRow.lane.bars[0]?.status).toBe('completed');
  expect(activityRow.lane.bars[0]?.entry.events).toHaveLength(2);
});

test('cycle guard catches a grandparent reference without requesting it', () => {
  expect(isWorkflowCycle(ROOT, [ROOT, CHILD])).toBe(true);
  const cyclingChildEntries = projectTimeline([childStarted(1, CHILD, ROOT, 2)]);
  const cyclingChildren = new Map<string, ChildTimelineState>([
    [childPath, { status: 'ready', entries: cyclingChildEntries, isRunning: true }],
  ]);
  const cyclePath = childNodePath(childPath, ROOT);
  const tree = flattenLaneTree(
    { workflowId: ROOT, entries: rootEntries, isRunning: true },
    cyclingChildren,
    new Set([childPath, cyclePath])
  );

  expect(tree.rows.some((row) => row.kind === 'notice' && row.notice === 'cycle')).toBe(true);
  expect(tree.requests.map((request) => request.path)).toEqual([childPath]);
});

test('SSR shape renders child lanes inside the one chart on the shared track origin', () => {
  const markup = renderToStaticMarkup(
    <Swimlane
      entries={rootEntries}
      initialChildTimelines={readyChildren()}
      initialExpandedChildren={[CHILD, GRANDCHILD]}
      isRunning={false}
      nowMs={Date.parse('2026-07-15T00:00:10Z')}
      onSelect={() => {}}
      selectedSequence={null}
      workflowId={ROOT as WorkflowId}
    />
  );

  expect(markup).toContain('data-workflow-id="child-1"');
  expect(markup).toContain('data-depth="1"');
  expect(markup).toContain('data-depth="2"');
  expect(markup).toContain('charge-card');
  expect(markup).not.toContain('child-run:');
});

test('depth bar selection carries the child workflow id and child timeline', () => {
  const activityBar = flattenLaneTree(
    { workflowId: ROOT, entries: rootEntries, isRunning: false },
    readyChildren(),
    new Set([childPath])
  ).rows.find(
    (row) => row.kind === 'lane' && row.workflowId === CHILD && row.lane.kind === 'activity'
  );
  if (
    activityBar === undefined ||
    activityBar.kind !== 'lane' ||
    activityBar.lane.bars[0] === undefined
  ) {
    throw new Error('expected a child activity bar');
  }

  const selection = selectionForBar(CHILD as WorkflowId, childEntries, activityBar.lane.bars[0], [
    activityBar.lane.bars[0].sequence,
  ]);
  expect(selection.workflowId).toBe(CHILD);
  expect(selection.timeline).toBe(childEntries);

  const surface = resolveSelectionSurface(
    ROOT as WorkflowId,
    rootEntries,
    selection.sequence,
    selection
  );
  expect(surface.workflowId).toBe(CHILD);
  expect(surface.timeline).toBe(childEntries);
  expect(surface.entry?.summary).toContain('charge-card');
});
