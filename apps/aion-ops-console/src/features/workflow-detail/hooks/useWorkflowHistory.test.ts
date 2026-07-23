import { expect, test } from 'bun:test';

import type { HistoryWindowRequest, RequestOptions } from '@/lib/api';
import type { Event, WorkflowId } from '@/types';

import { terminalOutcomeForEvents } from '../lib/timeline';
import {
  fetchEarlierWorkflowHistory,
  fetchInitialWorkflowHistory,
  fetchNextWorkflowHistory,
  HISTORY_WINDOW_SIZE,
  type WorkflowHistoryClient,
  type WorkflowHistorySnapshot,
} from './useWorkflowHistory';

const workflowId = '00000000-0000-0000-0000-000000000001' as WorkflowId;
const options: RequestOptions = { namespace: 'default' };

test('history discovery loads the terminal tail first with the fixed 500-event window', async () => {
  const calls: HistoryWindowRequest[] = [];
  const terminal = completed(999);
  const client = queuedClient(calls, [
    { events: [started(0)], nextFromSeq: 1, headSeq: 999 },
    { events: [started(500), terminal], nextFromSeq: null, headSeq: 999 },
  ]);

  const snapshot = await fetchInitialWorkflowHistory(client, workflowId, options);

  expect(calls).toEqual([
    { fromSeq: 0, limit: 1 },
    { fromSeq: 500, limit: HISTORY_WINDOW_SIZE },
  ]);
  expect(snapshot.loadedFromSeq).toBe(500);
  expect(snapshot.events.map(sequence)).toEqual([500, 999]);
  // Terminal projection scans the last terminal/reopened event, which is in the tail.
  expect(terminalOutcomeForEvents(snapshot.events)).toBe('completed');
});

test('backward paging requests the preceding bounded range and merges by sequence', async () => {
  const calls: HistoryWindowRequest[] = [];
  const current: WorkflowHistorySnapshot = {
    events: [started(500), started(700)],
    headSeq: 999,
    loadedFromSeq: 500,
    nextFromSeq: null,
  };
  const client = queuedClient(calls, [
    { events: [started(0), started(499), started(500)], nextFromSeq: 501, headSeq: 999 },
  ]);

  const snapshot = await fetchEarlierWorkflowHistory(client, workflowId, current, options);

  expect(calls).toEqual([{ fromSeq: 0, limit: 500 }]);
  expect(snapshot.loadedFromSeq).toBe(0);
  expect(snapshot.events.map(sequence)).toEqual([0, 499, 500, 700]);
});

test('forward paging fills a tail-read gap and preserves the original backward boundary', async () => {
  const calls: HistoryWindowRequest[] = [];
  const current: WorkflowHistorySnapshot = {
    events: [started(500), started(999)],
    headSeq: 1200,
    loadedFromSeq: 500,
    nextFromSeq: 1000,
  };
  const client = queuedClient(calls, [
    { events: [started(1000), started(1200)], nextFromSeq: null, headSeq: 1200 },
  ]);

  const snapshot = await fetchNextWorkflowHistory(client, workflowId, current, options);

  expect(calls).toEqual([{ fromSeq: 1000, limit: HISTORY_WINDOW_SIZE }]);
  expect(snapshot.loadedFromSeq).toBe(500);
  expect(snapshot.nextFromSeq).toBeNull();
  expect(snapshot.events.map(sequence)).toEqual([500, 999, 1000, 1200]);
});

function queuedClient(
  calls: HistoryWindowRequest[],
  windows: Awaited<ReturnType<WorkflowHistoryClient['getHistoryWindow']>>[]
): WorkflowHistoryClient {
  return {
    getHistoryWindow: async (_workflowId, window) => {
      calls.push(window);
      const response = windows.shift();
      if (response === undefined) {
        throw new Error('unexpected history request');
      }
      return response;
    },
  };
}

function started(seq: number): Event {
  return {
    type: 'WorkflowStarted',
    data: {
      envelope: {
        seq,
        recorded_at: '2026-07-01T00:00:00Z',
        workflow_id: workflowId,
      },
      workflow_type: 'window-test',
      input: { content_type: 'Json', bytes: [123, 125] },
      run_id: '00000000-0000-0000-0000-0000000000aa',
      parent_run_id: null,
      package_version: '1.0.0',
    },
  };
}

function completed(seq: number): Event {
  return {
    type: 'WorkflowCompleted',
    data: {
      envelope: {
        seq,
        recorded_at: '2026-07-01T00:00:01Z',
        workflow_id: workflowId,
      },
      result: { content_type: 'Json', bytes: [123, 125] },
    },
  };
}

function sequence(event: Event): number {
  return event.data.envelope.seq;
}
