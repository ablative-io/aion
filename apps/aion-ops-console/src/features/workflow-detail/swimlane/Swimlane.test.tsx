import { expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { MemoryRouter } from 'react-router';

import type { Event, Payload } from '@/types';

import { projectTimeline } from '../lib/timeline';
import { CycleNotice, isEmbedCycle } from './EmbeddedRunView';
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
  return {
    type: 'ActivityStarted',
    data: { envelope: envelope(seq), activity_id: id, attempt: 1 },
  };
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

function workflowFailed(seq: number, message: string): Event {
  return {
    type: 'WorkflowFailed',
    data: { envelope: envelope(seq), error: { message, details: null } },
  };
}

function terminalActivityFailed(seq: number, id: number, attempt: number): Event {
  return {
    type: 'ActivityFailed',
    data: {
      envelope: envelope(seq),
      activity_id: id,
      attempt,
      error: { kind: 'Terminal', message: 'terminal boom', details: null },
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

test('the reopen affordance appears for a terminal Failed run, not a fresh reopen event', () => {
  const failedHistory = [
    workflowStarted(1),
    activityScheduled(2, 10, 'charge'),
    terminalActivityFailed(3, 10, 1),
    workflowFailed(4, 'charge declined'),
  ];
  const withReopen = renderToStaticMarkup(
    <WorkflowDetailViewContent
      error={null}
      history={failedHistory}
      isError={false}
      isLive={false}
      isLoading={false}
      isTerminal={true}
      namespace="default"
      terminalOutcome="failed"
      timeline={projectTimeline(failedHistory)}
      workflowId={WORKFLOW_ID}
    />
  );
  // The Reopen affordance is gated on the terminal Failed/Cancelled outcome, NOT
  // on an already-recorded WorkflowReopened event (the old inverted gate).
  expect(withReopen).toContain('reopen-open');
  // The failure context (failed step + reason) is surfaced where reopen is decided.
  expect(withReopen).toContain('failure-context');
  expect(withReopen).toContain('charge');
  expect(withReopen).toContain('charge declined');

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
  // A running workflow shows neither the reopen affordance nor an empty failure panel.
  expect(withoutReopen).not.toContain('reopen-open');
  expect(withoutReopen).not.toContain('failure-context');
});

test('opening the reopen panel mounts ReopenDiff from the live history with an enabled commit', () => {
  const failedHistory = [
    workflowStarted(1),
    activityScheduled(2, 10, 'charge'),
    terminalActivityFailed(3, 10, 1),
    workflowFailed(4, 'charge declined'),
  ];
  const markup = renderToStaticMarkup(
    <WorkflowDetailViewContent
      error={null}
      history={failedHistory}
      initialReopenOpen={true}
      isError={false}
      isLive={false}
      isLoading={false}
      isTerminal={true}
      namespace="default"
      terminalOutcome="failed"
      timeline={projectTimeline(failedHistory)}
      workflowId={WORKFLOW_ID}
    />
  );

  expect(markup).toContain('reopen-diff');
  expect(markup).toContain('Reopen preview');
  // The commit button is present and enabled (no stale "not yet available" note).
  expect(markup).toContain('reopen-commit');
  expect(markup).not.toContain('not yet available');
});

test('child lane carries its child workflow id for expansion', () => {
  const layout = layoutSwimlane(projectTimeline([workflowStarted(1), childStarted(2, 'child-1')]));

  const childLane = layout.lanes.find((lane) => lane.kind === 'child');
  const lifecycleLane = layout.lanes.find((lane) => lane.kind === 'lifecycle');
  expect(childLane?.childWorkflowId).toBe('child-1');
  expect(lifecycleLane?.childWorkflowId).toBeNull();
});

test('swimlane renders an expand-run affordance only for child lanes', () => {
  const entries = projectTimeline([activityScheduled(1, 10, 'charge'), childStarted(2, 'child-1')]);
  const markup = renderToStaticMarkup(
    <Swimlane
      entries={entries}
      onSelect={() => {}}
      renderChildRun={() => null}
      selectedSequence={null}
    />
  );

  expect(markup).toContain('data-testid="expand-child:child-1"');
  expect(markup).toContain('aria-label="Expand child run child-1"');
  // Exactly ONE expand affordance: the activity lane offers none.
  expect(markup.match(/expand-child:/g)).toHaveLength(1);
});

test('the run-expand toggle renders INSIDE the fixed-width label cell (track alignment)', () => {
  const entries = projectTimeline([activityScheduled(1, 10, 'charge'), childStarted(2, 'child-1')]);
  const markup = renderToStaticMarkup(
    <Swimlane
      entries={entries}
      onSelect={() => {}}
      renderChildRun={() => null}
      selectedSequence={null}
    />
  );

  // The 168px label cell nests no <div>, so its first closing </div> ends the
  // cell. The toggle must sit before that close: rendered as a flex SIBLING of
  // the cell it would push the child lane's track ~17px right of the SeqAxis
  // dense-rank columns (every non-child track starts at LANE_LABEL_WIDTH).
  const toggleAt = markup.indexOf('data-testid="expand-child:child-1"');
  expect(toggleAt).toBeGreaterThan(-1);
  const cellStart = markup.lastIndexOf('w-[168px]', toggleAt);
  expect(cellStart).toBeGreaterThan(-1);
  const cellEnd = markup.indexOf('</div>', cellStart);
  expect(toggleAt).toBeLessThan(cellEnd);
});

test('an initially expanded child lane renders the injected run view beneath the chart', () => {
  const entries = projectTimeline([childStarted(1, 'child-1')]);
  const markup = renderToStaticMarkup(
    <Swimlane
      entries={entries}
      initialExpandedChildren={['child-1']}
      onSelect={() => {}}
      renderChildRun={(id) => <div data-testid={`stub-${id}`}>embedded-stub</div>}
      selectedSequence={null}
    />
  );

  expect(markup).toContain('data-testid="child-run:child-1"');
  expect(markup).toContain('embedded-stub');
  // The toggle reads expanded (its aria-label flips to Collapse).
  expect(markup).toContain('aria-label="Collapse child run child-1"');
});

test('no expansion affordance renders without a renderChildRun injection', () => {
  const entries = projectTimeline([childStarted(1, 'child-1')]);
  const markup = renderToStaticMarkup(
    <Swimlane
      entries={entries}
      initialExpandedChildren={['child-1']}
      onSelect={() => {}}
      selectedSequence={null}
    />
  );

  expect(markup).not.toContain('expand-child:');
  expect(markup).not.toContain('child-run:');
});

test('expanded lanes nest recursively to grandchild depth', () => {
  const parentEntries = projectTimeline([childStarted(1, 'child-1')]);
  const grandchildEntries = projectTimeline([childStarted(1, 'grandchild-1')]);
  const markup = renderToStaticMarkup(
    <Swimlane
      entries={parentEntries}
      initialExpandedChildren={['child-1']}
      onSelect={() => {}}
      renderChildRun={() => (
        <Swimlane
          entries={grandchildEntries}
          initialExpandedChildren={['grandchild-1']}
          onSelect={() => {}}
          renderChildRun={(id) => <div data-testid={`stub-${id}`}>grandchild-stub</div>}
          selectedSequence={null}
        />
      )}
      selectedSequence={null}
    />
  );

  expect(markup).toContain('data-testid="child-run:child-1"');
  expect(markup).toContain('data-testid="child-run:grandchild-1"');
  expect(markup).toContain('grandchild-stub');
});

test('embedded content renders the ancestry breadcrumb, status, and full-view link', () => {
  const timeline = projectTimeline([workflowStarted(1), activityScheduled(2, 10, 'charge')]);
  const markup = renderToStaticMarkup(
    <MemoryRouter>
      <WorkflowDetailViewContent
        ancestry={['parent-1']}
        embedded
        error={null}
        history={[]}
        isError={false}
        isLive={true}
        isLoading={false}
        isTerminal={false}
        namespace="default"
        terminalOutcome={null}
        timeline={timeline}
        workflowId="child-1"
      />
    </MemoryRouter>
  );

  expect(markup).toContain('aria-label="Workflow ancestry"');
  expect(markup).toContain('/workflows/parent-1');
  expect(markup).toContain('data-testid="open-full-view"');
  expect(markup).toContain('/workflows/child-1');
  // The intervention surface (attempt navigator) is present at depth.
  expect(markup).toContain('Agent attempts');
  // The page <h1> header does NOT render in embedded mode.
  expect(markup).not.toContain('Workflow child-1');
});

test('isEmbedCycle guards ancestor re-entry', () => {
  expect(isEmbedCycle('a', ['a'])).toBe(true);
  expect(isEmbedCycle('b', ['a'])).toBe(false);
  expect(isEmbedCycle('a', [])).toBe(false);

  const markup = renderToStaticMarkup(<CycleNotice workflowId="wf-cycle" />);
  expect(markup).toContain('data-testid="embed-cycle"');
  expect(markup).toContain('wf-cycle');
});
