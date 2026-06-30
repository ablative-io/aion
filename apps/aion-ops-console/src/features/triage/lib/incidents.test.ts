import { describe, expect, test } from 'bun:test';

import type { WorkflowStatus, WorkflowSummary } from '@/types';

import { GATED_FEEDS } from './gatedFeeds';
import { deriveWorkflowIncidents, STUCK_RUNNING_THRESHOLD_MS, sortIncidents } from './incidents';

const NOW = Date.parse('2026-06-29T12:00:00Z');

function summary(overrides: Partial<WorkflowSummary> & { workflow_id: string }): WorkflowSummary {
  return {
    workflow_type: 'collect_four',
    status: 'Running' as WorkflowStatus,
    started_at: '2026-06-29T11:59:00Z',
    ended_at: null,
    parent: null,
    ...overrides,
  };
}

describe('deriveWorkflowIncidents', () => {
  test('emits a failure incident for a Failed workflow that deep-links to its detail', () => {
    const incidents = deriveWorkflowIncidents(
      [summary({ workflow_id: 'wf-1', status: 'Failed', ended_at: '2026-06-29T11:30:00Z' })],
      NOW
    );

    expect(incidents).toHaveLength(1);
    expect(incidents[0]?.kind).toBe('workflow-failure');
    expect(incidents[0]?.target).toEqual({ kind: 'workflow-detail', workflowId: 'wf-1' });
  });

  test('emits a timeout incident for a TimedOut workflow', () => {
    const incidents = deriveWorkflowIncidents(
      [summary({ workflow_id: 'wf-t', status: 'TimedOut' })],
      NOW
    );

    expect(incidents).toHaveLength(1);
    expect(incidents[0]?.kind).toBe('workflow-failure');
    expect(incidents[0]?.title).toContain('timed out');
  });

  test('a long-running workflow becomes a stuck incident; a fresh one does not', () => {
    const stuckStart = new Date(NOW - STUCK_RUNNING_THRESHOLD_MS - 60_000).toISOString();
    const freshStart = new Date(NOW - 60_000).toISOString();

    const incidents = deriveWorkflowIncidents(
      [
        summary({ workflow_id: 'wf-stuck', status: 'Running', started_at: stuckStart }),
        summary({ workflow_id: 'wf-fresh', status: 'Running', started_at: freshStart }),
      ],
      NOW
    );

    expect(incidents).toHaveLength(1);
    expect(incidents[0]?.kind).toBe('workflow-stuck');
    expect(incidents[0]?.id).toBe('wf-stuck:wf-stuck');
  });

  test('failures outrank stuck-running workflows', () => {
    const stuckStart = new Date(NOW - STUCK_RUNNING_THRESHOLD_MS - 60_000).toISOString();

    const incidents = deriveWorkflowIncidents(
      [
        summary({ workflow_id: 'wf-stuck', status: 'Running', started_at: stuckStart }),
        summary({ workflow_id: 'wf-fail', status: 'Failed' }),
      ],
      NOW
    );

    expect(incidents.map((incident) => incident.kind)).toEqual([
      'workflow-failure',
      'workflow-stuck',
    ]);
  });

  test('Completed and Cancelled workflows produce no incidents (no fabrication)', () => {
    const incidents = deriveWorkflowIncidents(
      [
        summary({ workflow_id: 'wf-ok', status: 'Completed' }),
        summary({ workflow_id: 'wf-cancel', status: 'Cancelled' }),
      ],
      NOW
    );

    expect(incidents).toEqual([]);
  });

  test('an unparseable started_at never throws and never invents a stuck incident', () => {
    const incidents = deriveWorkflowIncidents(
      [summary({ workflow_id: 'wf-bad', status: 'Running', started_at: 'not-a-date' })],
      NOW
    );

    expect(incidents).toEqual([]);
  });
});

describe('sortIncidents', () => {
  test('is stable and descending by rank then recency', () => {
    const base = deriveWorkflowIncidents(
      [
        summary({
          workflow_id: 'a',
          status: 'Failed',
          ended_at: '2026-06-29T10:00:00Z',
        }),
        summary({
          workflow_id: 'b',
          status: 'Failed',
          ended_at: '2026-06-29T11:00:00Z',
        }),
      ],
      NOW
    );

    const sorted = sortIncidents(base);
    // Same rank class → more recent failure (b) first.
    expect(sorted[0]?.id).toBe('wf-fail:b');
  });
});

describe('gated-feed degradation', () => {
  test('all server-gated incident classes are reported as awaiting-support feeds, not emitted as incidents', () => {
    const kinds = GATED_FEEDS.map((feed) => feed.kind);
    expect(kinds).toEqual(['dead-worker', 'outbox-failed', 'fenced-rejection', 'shard-adoption']);
    // None of these can be produced from the list query, so the derive path never yields them.
    const incidents = deriveWorkflowIncidents(
      [summary({ workflow_id: 'x', status: 'Failed' })],
      NOW
    );
    for (const incident of incidents) {
      expect(kinds).not.toContain(incident.kind);
    }
  });
});
