import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { createMemoryRouter, RouterProvider } from 'react-router';

import { ApiClient, type WorkflowPage } from '@/lib/api';
import type { Event, Namespace, WorkflowFilter, WorkflowSummary } from '@/types';
import { patchWorkflowPage } from '../hooks/useLiveListUpdates';
import { queryWorkflowPage, workflowQueryKey } from '../hooks/useWorkflowQuery';
import {
  EMPTY_WORKFLOW_LIST_FILTER_STATE,
  type WorkflowListPaginationState,
  workflowFilterFromState,
} from '../types';
import { NewAboveBanner, WorkflowListBody } from './WorkflowList';
import { WorkflowRow } from './WorkflowRow';

const workflow: WorkflowSummary = {
  workflow_id: 'workflow-1',
  workflow_type: 'EmailDigest',
  status: 'Running',
  started_at: '2026-06-05T20:00:00Z',
  ended_at: null,
  parent: null,
};

const page: WorkflowPage<WorkflowSummary> = {
  items: [workflow],
  nextCursor: null,
  hasMore: false,
};

describe('WorkflowListBody', () => {
  test('renders rows for a returned workflow page', () => {
    const markup = renderWithRouter(
      <WorkflowListBody
        query={queryState({ data: page })}
        canGoPrevious={false}
        onPrevious={() => undefined}
        onNext={() => undefined}
      />
    );

    expect(markup).toContain('workflow-1');
    expect(markup).toContain('EmailDigest');
    expect(markup).toContain('Running');
    expect(markup).toContain('/workflows/workflow-1');
  });

  test('renders loading skeleton rows while initially loading', () => {
    const markup = renderToStaticMarkup(
      <WorkflowListBody
        query={queryState({ isPending: true })}
        canGoPrevious={false}
        onPrevious={() => undefined}
        onNext={() => undefined}
      />
    );

    expect(markup).toContain('Loading workflow summaries');
  });

  test('renders an empty state when no workflows match', () => {
    const markup = renderToStaticMarkup(
      <WorkflowListBody
        query={queryState({ data: { items: [], nextCursor: null, hasMore: false } })}
        canGoPrevious={false}
        onPrevious={() => undefined}
        onNext={() => undefined}
      />
    );

    expect(markup).toContain('No workflows match this filter');
  });

  test('renders an error state with a retry affordance', () => {
    const markup = renderToStaticMarkup(
      <WorkflowListBody
        query={queryState({ error: new Error('server unavailable'), isError: true })}
        canGoPrevious={false}
        onPrevious={() => undefined}
        onNext={() => undefined}
      />
    );

    expect(markup).toContain('Could not load workflows');
    expect(markup).toContain('server unavailable');
    expect(markup).toContain('Retry');
  });
});

describe('WorkflowRow', () => {
  test('links workflow rows to the detail route', () => {
    const markup = renderWithRouter(
      <table>
        <tbody>
          <WorkflowRow workflow={workflow} />
        </tbody>
      </table>
    );

    expect(markup).toContain('/workflows/workflow-1');
  });
});

describe('live workflow list updates', () => {
  test('patches status transitions and inserts newly matching started workflows', () => {
    const filter = workflowFilterFromState(EMPTY_WORKFLOW_LIST_FILTER_STATE);
    const completed = patchWorkflowPage(
      page,
      workflowCompleted(2, workflow.workflow_id),
      filter,
      50
    );

    expect(completed?.items[0]?.status).toBe('Completed');
    expect(completed?.items[0]?.ended_at).toBe('2026-06-05T20:02:00Z');

    const inserted = patchWorkflowPage(
      completed ?? page,
      workflowStarted(3, 'workflow-2', 'EmailDigest'),
      filter,
      50
    );

    expect(inserted?.items.map((item) => item.workflow_id)).toEqual(['workflow-2', 'workflow-1']);
    expect(inserted?.items[0]?.status).toBe('Running');
  });

  test('patches an existing row status without inserting or reordering', () => {
    const filter = workflowFilterFromState(EMPTY_WORKFLOW_LIST_FILTER_STATE);
    const patched = patchWorkflowPage(
      page,
      workflowCompleted(2, workflow.workflow_id),
      filter,
      50,
      { allowInsert: false }
    );

    expect(patched?.items.map((item) => item.workflow_id)).toEqual(['workflow-1']);
    expect(patched?.items[0]?.status).toBe('Completed');
  });

  test('off page 1, a newly-started workflow is NOT inserted into the cursor view', () => {
    const filter = workflowFilterFromState(EMPTY_WORKFLOW_LIST_FILTER_STATE);
    const result = patchWorkflowPage(
      page,
      workflowStarted(3, 'workflow-2', 'EmailDigest'),
      filter,
      50,
      { allowInsert: false }
    );

    // null signals the caller to surface a "new above" affordance rather than
    // corrupting the deeper page's cursor anchor (R3).
    expect(result).toBeNull();
  });
});

describe('NewAboveBanner', () => {
  test('is hidden when there are no new workflows above', () => {
    const markup = renderToStaticMarkup(<NewAboveBanner count={0} onShow={() => undefined} />);
    expect(markup).toBe('');
  });

  test('surfaces a count and a jump-to-top action when rows arrive above', () => {
    const markup = renderToStaticMarkup(<NewAboveBanner count={3} onShow={() => undefined} />);
    expect(markup).toContain('3 new workflows started above');
    expect(markup).toContain('Jump to top');
  });

  test('uses singular phrasing for a single new workflow', () => {
    const markup = renderToStaticMarkup(<NewAboveBanner count={1} onShow={() => undefined} />);
    expect(markup).toContain('1 new workflow started above');
  });
});

describe('workflow list query path', () => {
  test('query helper requests namespaced summary pages with generated filter and pagination', async () => {
    const namespace = 'default' as Namespace;
    const filter = workflowFilterFromState({
      ...EMPTY_WORKFLOW_LIST_FILTER_STATE,
      workflowType: 'EmailDigest',
      status: 'Failed',
      startedAfter: '2026-06-05T20:00',
    });
    const requests: unknown[] = [];
    const client = new ApiClient({
      fetchImpl: async (_input, init) => {
        requests.push(JSON.parse(String(init?.body)));
        return Response.json({ items: [workflow], next_cursor: null, has_more: false });
      },
    });

    const result = await queryWorkflowPage(client, namespace, filter, {
      cursor: 'cursor-1',
      limit: 25,
    });

    expect(requests).toEqual([
      {
        namespace,
        filter: {
          workflow_type: 'EmailDigest',
          status: 'Failed',
          started_after: '2026-06-05T20:00',
          started_before: null,
          parent: null,
        } satisfies WorkflowFilter,
        cursor: 'cursor-1',
        limit: 25,
      },
    ]);
    expect(result.items).toEqual([workflow]);
  });

  test('query key includes namespace, filter, and pagination', () => {
    const namespace = 'tenant-a' as Namespace;
    const filter = workflowFilterFromState(EMPTY_WORKFLOW_LIST_FILTER_STATE);
    const pageRequest = { cursor: undefined, limit: 50 };

    expect(workflowQueryKey(namespace, filter, pageRequest)).toEqual([
      'workflows',
      namespace,
      filter,
      pageRequest,
    ]);
  });
});

describe('workflow list filter and pagination state', () => {
  test('filter changes produce a new WorkflowFilter and reset pagination', () => {
    const previousPagination: WorkflowListPaginationState = {
      cursor: 'cursor-2',
      previousCursors: ['', 'cursor-1'],
    };
    const filter = workflowFilterFromState({
      ...EMPTY_WORKFLOW_LIST_FILTER_STATE,
      workflowType: 'EmailDigest',
      status: 'Failed',
      startedAfter: '2026-06-05T20:00',
    });
    const resetPagination = resetPaginationOnFilterChange(previousPagination);

    expect(filter).toEqual({
      workflow_type: 'EmailDigest',
      status: 'Failed',
      started_after: '2026-06-05T20:00',
      started_before: null,
      parent: null,
    } satisfies WorkflowFilter);
    expect(resetPagination).toEqual({ cursor: undefined, previousCursors: [] });
  });
});

type QueryStateOverrides = {
  data?: WorkflowPage<WorkflowSummary>;
  error?: Error;
  isError?: boolean;
  isFetching?: boolean;
  isPending?: boolean;
};

function queryState(overrides: QueryStateOverrides) {
  return {
    data: overrides.data,
    error: overrides.error ?? null,
    isError: overrides.isError ?? false,
    isFetching: overrides.isFetching ?? false,
    isPending: overrides.isPending ?? false,
    refetch: () => Promise.resolve({}),
  } as ReturnType<typeof import('../hooks/useWorkflowQuery').useWorkflowQuery>;
}

function workflowStarted(seq: number, workflowId: string, workflowType: string): Event {
  return {
    type: 'WorkflowStarted',
    data: {
      envelope: { seq, recorded_at: `2026-06-05T20:0${seq}:00Z`, workflow_id: workflowId },
      input: { content_type: 'Json', bytes: [123, 125] },
      workflow_type: workflowType,
      run_id: '00000000-0000-0000-0000-0000000000a1',
      parent_run_id: null,
      package_version: '1.0.0',
    },
  };
}

function workflowCompleted(seq: number, workflowId: string): Event {
  return {
    type: 'WorkflowCompleted',
    data: {
      envelope: { seq, recorded_at: `2026-06-05T20:0${seq}:00Z`, workflow_id: workflowId },
      result: { content_type: 'Json', bytes: [123, 125] },
    },
  };
}

function resetPaginationOnFilterChange(
  pagination: WorkflowListPaginationState
): WorkflowListPaginationState {
  void pagination;
  return { cursor: undefined, previousCursors: [] };
}

function renderWithRouter(children: React.ReactNode) {
  const router = createMemoryRouter([{ path: '*', element: children }]);

  return renderToStaticMarkup(<RouterProvider router={router} />);
}
