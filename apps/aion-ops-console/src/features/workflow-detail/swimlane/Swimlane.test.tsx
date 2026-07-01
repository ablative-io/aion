import { expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import type { Event, Payload } from '@/types';

import { projectTimeline } from '../lib/timeline';
import { buildRankIndex, layoutSwimlane } from './laneLayout';
import { Swimlane } from './Swimlane';
import { WorkflowDetailViewContent } from './WorkflowDetailView';

const WORKFLOW_ID = 'workflow-1';
const PAYLOAD: Payload = { content_type: 'Json', bytes: [123, 125] };

function envelope(seq: number) {
  return {
    seq,
    recorded_at: `2026-06-29T00:00:${String(seq).padStart(2, '0')}Z`,
    workflow_id: WORKFLOW_ID,
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

function activityScheduled(seq: number, id: number, type: string): Event {
  return {
    type: 'ActivityScheduled',
    data: {
      envelope: envelope(seq),
      activity_id: id,
      activity_type: type,
      input: PAYLOAD,
      task_queue: 'default',
      node: null,
    },
  };
}

function activityStarted(seq: number, id: number): Event {
  return { type: 'ActivityStarted', data: { envelope: envelope(seq), activity_id: id, attempt: 1 } };
}

function activityFailed(seq: number, id: number, attempt: number): Event {
  return {
    type: 'ActivityFailed',
    data: {
      envelope: envelope(seq),
      activity_id: id,
      attempt,
      error: { kind: 'Retryable', message: 'boom', details: null },
    },
  };
}

function activityCompleted(seq: number, id: number): Event {
  return {
    type: 'ActivityCompleted',
    data: { envelope: envelope(seq), activity_id: id, attempt: 1, result: PAYLOAD },
  };
}

function timerStarted(seq: number, name: string): Event {
  return {
    type: 'TimerStarted',
    data: { envelope: envelope(seq), timer_id: { Named: name }, fire_at: '2026-06-29T01:00:00Z' },
  };
}

function timerFired(seq: number, name: string): Event {
  return { type: 'TimerFired', data: { envelope: envelope(seq), timer_id: { Named: name } } };
}

function workflowReopened(seq: number, reopened: number[]): Event {
  return {
    type: 'WorkflowReopened',
    data: {
      envelope: envelope(seq),
      run_id: '00000000-0000-0000-0000-0000000000a2',
      reopened,
    },
  };
}

function childStarted(seq: number, childId: string): Event {
  return {
    type: 'ChildWorkflowStarted',
    data: {
      envelope: envelope(seq),
      child_workflow_id: childId,
      workflow_type: 'receipt',
      input: PAYLOAD,
      package_version: '1.0.0',
    },
  };
}

test('buildRankIndex assigns dense ranks over all carried sequences', () => {
  const entries = projectTimeline([workflowStarted(1), timerStarted(5, 't1'), timerFired(9, 't1')]);
  const index = buildRankIndex(entries);

  expect(index.get(1)).toBe(0);
  expect(index.get(5)).toBe(1);
  expect(index.get(9)).toBe(2);
});

test('lane assignment yields one lane per correlation key plus lifecycle', () => {
  const layout = layoutSwimlane(
    projectTimeline([
      workflowStarted(1),
      activityScheduled(2, 10, 'charge'),
      activityCompleted(3, 10),
      timerStarted(4, 't1'),
      timerFired(5, 't1'),
    ])
  );

  const kinds = layout.lanes.map((lane) => lane.kind);
  expect(kinds).toContain('lifecycle');
  expect(kinds.filter((kind) => kind === 'activity')).toHaveLength(1);
  expect(kinds.filter((kind) => kind === 'timer')).toHaveLength(1);
});

test('attempt segmentation records one rank per ActivityFailed attempt', () => {
  const layout = layoutSwimlane(
    projectTimeline([
      activityScheduled(1, 10, 'charge'),
      activityStarted(2, 10),
      activityFailed(3, 10, 1),
      activityFailed(4, 10, 2),
      activityCompleted(5, 10),
    ])
  );

  const activityLane = layout.lanes.find((lane) => lane.kind === 'activity');
  const bar = activityLane?.bars[0];
  expect(bar?.attemptRanks).toHaveLength(2);
  expect(bar?.status).toBe('completed');
});

test('concurrent activity + timer overlap on the same rank span', () => {
  const layout = layoutSwimlane(
    projectTimeline([
      activityScheduled(1, 10, 'charge'),
      timerStarted(2, 't1'),
      activityCompleted(4, 10),
      timerFired(3, 't1'),
    ])
  );

  const activity = layout.lanes.find((lane) => lane.kind === 'activity')?.bars[0];
  const timer = layout.lanes.find((lane) => lane.kind === 'timer')?.bars[0];

  if (activity === undefined || timer === undefined) {
    throw new Error('expected both an activity and a timer lane');
  }

  // Their rank spans overlap (concurrent) rather than being sequential.
  expect(Math.max(activity.startRank, timer.startRank)).toBeLessThanOrEqual(
    Math.min(activity.endRank, timer.endRank)
  );
});

test('child bar carries a drill-link target and the swimlane renders it', () => {
  const entries = projectTimeline([childStarted(1, 'child-1')]);
  const layout = layoutSwimlane(entries);
  const childBar = layout.lanes.find((lane) => lane.kind === 'child')?.bars[0];

  expect(childBar?.childWorkflowId).toBe('child-1');

  const markup = renderToStaticMarkup(
    <Swimlane entries={entries} onSelect={() => {}} selectedSequence={null} />
  );
  expect(markup).toContain('Workflow swimlane');
  // The child lane renders with its drill affordance (↳) and the bar references
  // the child workflow in its accessible label.
  expect(markup).toContain('↳');
  expect(markup).toContain('seq 1');
});

test('live-append extends an activity bar in place (endRank grows)', () => {
  const initial = layoutSwimlane(projectTimeline([activityScheduled(1, 10, 'charge')]));
  const initialBar = initial.lanes.find((lane) => lane.kind === 'activity')?.bars[0];
  expect(initialBar?.endRank).toBe(0);

  const extended = layoutSwimlane(
    projectTimeline([activityScheduled(1, 10, 'charge'), activityCompleted(2, 10)])
  );
  const extendedBar = extended.lanes.find((lane) => lane.kind === 'activity')?.bars[0];
  expect(extendedBar?.endRank).toBe(1);
  expect(extendedBar?.status).toBe('completed');
});

test('WorkflowDetailViewContent renders swimlane mode by default with no fabricated data', () => {
  const timeline = projectTimeline([workflowStarted(1), activityScheduled(2, 10, 'charge')]);
  const markup = renderToStaticMarkup(
    <WorkflowDetailViewContent
      error={null}
      history={[]}
      isError={false}
      isLive={true}
      isLoading={false}
      isTerminal={false}
      namespace="default"
      terminalOutcome={null}
      timeline={timeline}
      workflowId={WORKFLOW_ID}
    />
  );

  expect(markup).toContain('Workflow swimlane');
  expect(markup).toContain('aria-pressed="true"');
});

test('WorkflowDetailViewContent surfaces error state (no silent failure)', () => {
  const markup = renderToStaticMarkup(
    <WorkflowDetailViewContent
      error={new Error('history failed')}
      history={[]}
      isError={true}
      isLive={false}
      isLoading={false}
      isTerminal={false}
      namespace="default"
      terminalOutcome={null}
      timeline={[]}
      workflowId={WORKFLOW_ID}
    />
  );

  expect(markup).toContain('history failed');
});

test('WorkflowDetailViewContent renders the loading skeleton while history is pending', () => {
  const markup = renderToStaticMarkup(
    <WorkflowDetailViewContent
      error={null}
      history={[]}
      isError={false}
      isLive={false}
      isLoading={true}
      isTerminal={false}
      namespace="default"
      terminalOutcome={null}
      timeline={[]}
      workflowId={WORKFLOW_ID}
    />
  );

  expect(markup).toContain('Loading timeline');
});

test('WorkflowDetailViewContent renders the empty state for a history with no events', () => {
  const markup = renderToStaticMarkup(
    <WorkflowDetailViewContent
      error={null}
      history={[]}
      isError={false}
      isLive={false}
      isLoading={false}
      isTerminal={false}
      namespace="default"
      terminalOutcome={null}
      timeline={[]}
      workflowId={WORKFLOW_ID}
    />
  );

  expect(markup).toContain('This workflow has no events yet');
});

test('WorkflowDetailViewContent renders the no-namespace state without an unscoped query', () => {
  const markup = renderToStaticMarkup(
    <WorkflowDetailViewContent
      error={null}
      history={[]}
      isError={false}
      isLive={false}
      isLoading={false}
      isTerminal={false}
      namespace={null}
      terminalOutcome={null}
      timeline={[]}
      workflowId={WORKFLOW_ID}
    />
  );

  expect(markup).toContain('No namespace selected');
});

test('the reopen-diff trigger appears only when history carries a WorkflowReopened event', () => {
  const reopenedHistory = [
    activityScheduled(1, 10, 'charge'),
    activityFailed(2, 10, 1),
    workflowReopened(3, [10]),
  ];
  const withReopen = renderToStaticMarkup(
    <WorkflowDetailViewContent
      error={null}
      history={reopenedHistory}
      isError={false}
      isLive={false}
      isLoading={false}
      isTerminal={true}
      namespace="default"
      terminalOutcome={null}
      timeline={projectTimeline(reopenedHistory)}
      workflowId={WORKFLOW_ID}
    />
  );
  expect(withReopen).toContain('Reopen diff');

  const cleanHistory = [workflowStarted(1), activityScheduled(2, 10, 'charge')];
  const withoutReopen = renderToStaticMarkup(
    <WorkflowDetailViewContent
      error={null}
      history={cleanHistory}
      isError={false}
      isLive={true}
      isLoading={false}
      isTerminal={false}
      namespace="default"
      terminalOutcome={null}
      timeline={projectTimeline(cleanHistory)}
      workflowId={WORKFLOW_ID}
    />
  );
  expect(withoutReopen).not.toContain('Reopen diff');
});

test('opening the reopen-diff mounts the ReopenDiff panel from the live history', () => {
  const reopenedHistory = [
    activityScheduled(1, 10, 'charge'),
    activityFailed(2, 10, 1),
    workflowReopened(3, [10]),
  ];
  const markup = renderToStaticMarkup(
    <WorkflowDetailViewContent
      error={null}
      history={reopenedHistory}
      initialReopenOpen={true}
      isError={false}
      isLive={false}
      isLoading={false}
      isTerminal={true}
      namespace="default"
      terminalOutcome={null}
      timeline={projectTimeline(reopenedHistory)}
      workflowId={WORKFLOW_ID}
    />
  );

  expect(markup).toContain('reopen-diff');
  expect(markup).toContain('Reopen preview');
});
