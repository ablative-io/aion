import { expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import type { Event, Payload } from '@/types';

import { projectTimeline } from '../lib/timeline';
import { layoutSwimlane } from './laneLayout';
import { Scrubber } from './Scrubber';
import { Swimlane } from './Swimlane';
import { prefixUpTo, scrubSequences } from './scrub';

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
  return { type: 'ActivityStarted', data: { envelope: envelope(seq), activity_id: id } };
}

function activityCompleted(seq: number, id: number): Event {
  return {
    type: 'ActivityCompleted',
    data: { envelope: envelope(seq), activity_id: id, result: PAYLOAD },
  };
}

function fullRun() {
  return projectTimeline([
    workflowStarted(1),
    activityScheduled(2, 10, 'charge'),
    activityStarted(3, 10),
    activityCompleted(4, 10),
  ]);
}

test('scrubSequences lists every recorded seq, ascending and distinct', () => {
  expect(scrubSequences(fullRun())).toEqual([1, 2, 3, 4]);
});

test('prefixUpTo(null) is the full live timeline (release returns to live)', () => {
  const timeline = fullRun();
  const prefix = prefixUpTo(timeline, null);

  expect(prefix).toHaveLength(timeline.length);
  expect(scrubSequences(prefix)).toEqual([1, 2, 3, 4]);
});

test('scrubbing back hides later events (the activity bar disappears)', () => {
  // At seq 1 only WorkflowStarted is recorded; the activity is not yet scheduled.
  const prefix = prefixUpTo(fullRun(), 1);
  const layout = layoutSwimlane(prefix);

  expect(layout.lanes.some((lane) => lane.kind === 'activity')).toBe(false);
  expect(layout.lanes.some((lane) => lane.kind === 'lifecycle')).toBe(true);
});

test('prefix re-derives status at the cut: a completed activity reads as started', () => {
  // Cut at seq 3 (ActivityStarted) — completion at seq 4 is excluded.
  const entry = prefixUpTo(fullRun(), 3).find((candidate) => candidate.kind === 'activity');

  if (entry === undefined || entry.kind !== 'activity') {
    throw new Error('expected an activity entry at the cut');
  }

  expect(entry.status).toBe('started');
  expect(entry.completed).toBeNull();
  // No fabricated values: the bar carries only the events at/below the cut.
  expect(entry.events.every((event) => event.data.envelope.seq <= 3)).toBe(true);
});

test('handle maps the far-right stop to live; an interior seq stays scrubbed', () => {
  const timeline = fullRun();

  const live = renderToStaticMarkup(
    <Scrubber entries={timeline} onScrub={() => {}} scrubSeq={null} />
  );
  expect(live).toContain('live · seq 4');
  // Live button is disabled when already live (no dead control).
  expect(live).toContain('disabled');

  const scrubbed = renderToStaticMarkup(
    <Scrubber entries={timeline} onScrub={() => {}} scrubSeq={2} />
  );
  expect(scrubbed).toContain('seq 2');
  expect(scrubbed).toContain('of 4');
});

test('Swimlane honours scrubSeq and renders only the prefix bars', () => {
  const timeline = fullRun();

  const scrubbed = renderToStaticMarkup(
    <Swimlane entries={timeline} onSelect={() => {}} scrubSeq={1} selectedSequence={null} />
  );
  // The activity bar (scheduled at seq 2) must not appear at the seq-1 cut.
  expect(scrubbed).toContain('Workflow swimlane');
  expect(scrubbed).not.toContain('charge');
});

test('empty timeline yields no scrubber (no fabricated stops)', () => {
  const markup = renderToStaticMarkup(<Scrubber entries={[]} onScrub={() => {}} scrubSeq={null} />);
  expect(markup).toBe('');
});
