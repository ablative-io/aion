import { expect, test } from 'bun:test';

import type { Event } from '@/types';

import { projectTimeline } from '../lib/timeline';
import { layoutSwimlane } from './laneLayout';
import { selectionForBar } from './Swimlane';
import { prefixUpTo } from './scrub';
import { resolveSelectionSurface } from './selectionSurface';

const WORKFLOW_ID = 'workflow-1';

function envelope(seq: number) {
  return {
    seq,
    recorded_at: `2026-06-29T00:00:${String(seq).padStart(2, '0')}Z`,
    workflow_id: WORKFLOW_ID,
  };
}

function timerStarted(seq: number, name: string): Event {
  return {
    type: 'TimerStarted',
    data: {
      envelope: envelope(seq),
      timer_id: { Named: name },
      fire_at: '2026-06-29T01:00:00Z',
    },
  };
}

function timerFired(seq: number, name: string): Event {
  return { type: 'TimerFired', data: { envelope: envelope(seq), timer_id: { Named: name } } };
}

function timerEntries() {
  return projectTimeline([
    timerStarted(1, 'retry-1'),
    timerStarted(2, 'retry-2'),
    timerStarted(3, 'deadline'),
    timerFired(4, 'retry-1'),
    timerFired(5, 'retry-2'),
    timerFired(6, 'deadline'),
  ]);
}

test('timer entries default to one counted aggregate and expand to one lane per timer', () => {
  const entries = timerEntries();

  const timerLanes = layoutSwimlane(entries).lanes.filter((lane) => lane.kind === 'timer');
  expect(timerLanes).toHaveLength(1);
  expect(timerLanes[0]?.id).toBe('timers');
  expect(timerLanes[0]?.label).toBe('Timers (3)');
  expect(timerLanes[0]?.bars).toHaveLength(3);

  const expandedTimerLanes = layoutSwimlane(entries, { expandTimers: true }).lanes.filter(
    (lane) => lane.kind === 'timer'
  );
  expect(expandedTimerLanes).toHaveLength(3);
  expect(expandedTimerLanes.map((lane) => lane.label)).toEqual([
    'Timer retry-1',
    'Timer retry-2',
    'Timer deadline',
  ]);
});

test('every grouped timer bar keeps its individual sequence selection and URL round-trip', () => {
  const entries = timerEntries();
  const timerLane = layoutSwimlane(entries).lanes.find((lane) => lane.kind === 'timer');
  if (timerLane === undefined) {
    throw new Error('expected a grouped timer lane');
  }

  for (const bar of timerLane.bars) {
    const selection = selectionForBar(WORKFLOW_ID, entries, bar, [bar.sequence]);
    const params = new URLSearchParams();
    params.set('seq', String(selection.sequence));
    const restoredSequence = Number(params.get('seq'));
    const surface = resolveSelectionSurface(WORKFLOW_ID, entries, restoredSequence, selection);

    expect(restoredSequence).toBe(bar.sequence);
    expect(surface.entry?.id).toBe(bar.entry.id);
  }
});

test('a scrub-cut timer aggregate is the union of the scrub-cut individual timer lanes', () => {
  const scrubbedEntries = prefixUpTo(timerEntries(), 4);
  const grouped = layoutSwimlane(scrubbedEntries).lanes.find((lane) => lane.kind === 'timer');
  const individual = layoutSwimlane(scrubbedEntries, { expandTimers: true }).lanes.filter(
    (lane) => lane.kind === 'timer'
  );
  if (grouped === undefined) {
    throw new Error('expected a scrubbed timer aggregate');
  }

  const groupedBars = grouped.bars.map(timerBarSnapshot).sort();
  const individualBars = individual
    .flatMap((lane) => lane.bars)
    .map(timerBarSnapshot)
    .sort();
  expect(groupedBars).toEqual(individualBars);
  expect(grouped.label).toBe('Timers (3)');
});

function timerBarSnapshot(bar: { id: string; startRank: number; endRank: number; status: string }) {
  return `${bar.id}:${bar.startRank}-${bar.endRank}:${bar.status}`;
}
