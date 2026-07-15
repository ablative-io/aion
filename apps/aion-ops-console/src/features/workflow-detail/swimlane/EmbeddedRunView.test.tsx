import { describe, expect, test } from 'bun:test';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { renderToStaticMarkup } from 'react-dom/server';
import { MemoryRouter } from 'react-router';

import { NamespaceProvider } from '@/features/namespace';
import type { ApiClient } from '@/lib/api';
import type { Event, Namespace, Payload, WorkflowId } from '@/types';

import { workflowHistoryQueryKey } from '../hooks/useWorkflowHistory';
import { EmbeddedRunView } from './EmbeddedRunView';

/**
 * EXECUTION proof for the real embedded run view (past the cycle guard): these
 * tests SSR-render {@link EmbeddedRunView} itself — not a stub renderChildRun —
 * under MemoryRouter + QueryClientProvider + NamespaceProvider with the
 * workflow-history query caches SEEDED, so `useWorkflowHistory` resolves the
 * durable backfill synchronously and `EmbeddedRunViewBody` actually runs:
 *
 * - an expand-late backfill renders the child's FULL durable history,
 * - the recursion accumulates ancestry (`[...ancestry, workflowId]`) so a
 *   grandchild's breadcrumb carries every ancestor, and
 * - a grandparent cycle at depth is caught by the accumulated chain (a
 *   back-reference renders {@link CycleNotice} instead of re-embedding).
 */

const NAMESPACE = 'default' as Namespace;
const PAYLOAD: Payload = { content_type: 'Json', bytes: [123, 125] };

const namespaceClient = {
  listNamespaces: async () => [NAMESPACE],
} as unknown as Pick<ApiClient, 'listNamespaces'>;

function envelope(seq: number, workflowId: string) {
  return {
    seq,
    recorded_at: `2026-06-29T00:00:${String(seq).padStart(2, '0')}Z`,
    workflow_id: workflowId,
  };
}

function workflowStarted(seq: number, workflowId: string): Event {
  return {
    type: 'WorkflowStarted',
    data: {
      envelope: envelope(seq, workflowId),
      workflow_type: 'checkout',
      input: PAYLOAD,
      run_id: '00000000-0000-0000-0000-0000000000a1',
      parent_run_id: null,
      package_version: '1.0.0',
    },
  };
}

function activityScheduled(seq: number, workflowId: string, type: string): Event {
  return {
    type: 'ActivityScheduled',
    data: {
      envelope: envelope(seq, workflowId),
      activity_id: 10,
      activity_type: type,
      input: PAYLOAD,
      task_queue: 'default',
      node: null,
    },
  };
}

function childStarted(seq: number, workflowId: string, childId: string): Event {
  return {
    type: 'ChildWorkflowStarted',
    data: {
      envelope: envelope(seq, workflowId),
      child_workflow_id: childId,
      workflow_type: 'receipt',
      input: PAYLOAD,
      package_version: '1.0.0',
    },
  };
}

type Seed = { workflowId: string; events: Event[] };

function renderEmbedded(options: {
  workflowId: string;
  ancestry: readonly string[];
  seeds: readonly Seed[];
  initialExpandedChildren?: readonly string[];
}) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  for (const seed of options.seeds) {
    queryClient.setQueryData(
      workflowHistoryQueryKey(NAMESPACE, seed.workflowId as WorkflowId),
      seed.events
    );
  }

  return renderToStaticMarkup(
    <QueryClientProvider client={queryClient}>
      <NamespaceProvider apiClient={namespaceClient} initialNamespace={NAMESPACE}>
        <MemoryRouter>
          <EmbeddedRunView
            ancestry={options.ancestry}
            initialExpandedChildren={options.initialExpandedChildren}
            namespace={NAMESPACE}
            workflowId={options.workflowId as WorkflowId}
          />
        </MemoryRouter>
      </NamespaceProvider>
    </QueryClientProvider>
  );
}

describe('EmbeddedRunView (real body, seeded durable history)', () => {
  test('an expand-late mount renders the child FULL durable history from the backfill', () => {
    const markup = renderEmbedded({
      ancestry: ['root-1'],
      seeds: [
        {
          workflowId: 'child-1',
          events: [
            workflowStarted(1, 'child-1'),
            activityScheduled(2, 'child-1', 'charge-card'),
            childStarted(3, 'child-1', 'grand-1'),
          ],
        },
      ],
      workflowId: 'child-1',
    });

    // The body ran: no loading skeleton, no empty state — the seeded durable
    // history reached the swimlane (dropping useWorkflowHistory, forcing
    // timeline={[]}, or breaking the enabled gate all fail here).
    expect(markup).not.toContain('Loading timeline');
    expect(markup).not.toContain('no events yet');
    expect(markup).toContain('Workflow swimlane');
    expect(markup).toContain('charge-card');
    // The embedded chrome renders around the real data: ancestry trail + escape
    // hatch to the child's own full view.
    expect(markup).toContain('aria-label="Workflow ancestry"');
    expect(markup).toContain('/workflows/root-1');
    expect(markup).toContain('data-testid="open-full-view"');
    expect(markup).toContain('/workflows/child-1');
    // The child's own child lane offers the recursive expand affordance.
    expect(markup).toContain('data-testid="expand-child:grand-1"');
  });

  test('an unseeded history renders the loading skeleton (the query gate is real)', () => {
    const markup = renderEmbedded({
      ancestry: ['root-1'],
      seeds: [],
      workflowId: 'child-1',
    });

    expect(markup).toContain('Loading timeline');
    expect(markup).not.toContain('Workflow swimlane');
  });

  test('grandchild recursion runs the REAL EmbeddedRunView and accumulates ancestry', () => {
    const markup = renderEmbedded({
      ancestry: ['root-1'],
      initialExpandedChildren: ['grand-1'],
      seeds: [
        {
          workflowId: 'child-1',
          events: [workflowStarted(1, 'child-1'), childStarted(2, 'child-1', 'grand-1')],
        },
        {
          workflowId: 'grand-1',
          events: [workflowStarted(1, 'grand-1'), activityScheduled(2, 'grand-1', 'ship-parcel')],
        },
      ],
      workflowId: 'child-1',
    });

    // The expanded lane mounted the REAL nested EmbeddedRunView, which read the
    // GRANDCHILD's seeded durable history (not the parent's, not a stub).
    expect(markup).toContain('data-testid="child-run:grand-1"');
    expect(markup).toContain('ship-parcel');
    // Ancestry accumulated as [...ancestry, workflowId]: the grandchild's
    // breadcrumb links BOTH ancestors, so /workflows/root-1 appears in the
    // outer trail AND the nested one. (The `[workflowId]`-only mutation would
    // leave exactly one.)
    expect(markup.match(/\/workflows\/root-1/g)?.length).toBe(2);
    expect(markup.match(/\/workflows\/child-1/g)?.length).toBeGreaterThanOrEqual(2);
  });

  test('a grandparent back-reference at depth renders CycleNotice, not a re-embed', () => {
    const markup = renderEmbedded({
      ancestry: ['root-1'],
      // Expand child-1 → grand-1, then grand-1 → child-1 (the back-reference).
      initialExpandedChildren: ['grand-1', 'child-1'],
      seeds: [
        {
          workflowId: 'child-1',
          events: [workflowStarted(1, 'child-1'), childStarted(2, 'child-1', 'grand-1')],
        },
        {
          workflowId: 'grand-1',
          // grand-1 claims child-1 (its own GRANDPARENT-level ancestor) as a child.
          events: [workflowStarted(1, 'grand-1'), childStarted(2, 'grand-1', 'child-1')],
        },
      ],
      workflowId: 'child-1',
    });

    // The accumulated chain ['root-1','child-1','grand-1'] caught the re-entry:
    // the depth-3 embed is the cycle notice, not another swimlane. (Passing
    // `[workflowId]` instead of `[...ancestry, workflowId]` misses this and
    // recurses forever — this render terminating AND noticing is the proof.)
    expect(markup).toContain('data-testid="embed-cycle"');
    expect(markup).toContain('Recursive child reference');
    expect(markup).toContain('Workflow child-1 is already expanded above');
  });
});
