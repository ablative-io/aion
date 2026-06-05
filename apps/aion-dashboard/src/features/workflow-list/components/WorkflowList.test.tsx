import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import type { WorkflowPage } from '@/lib/api';
import type { WorkflowFilter, WorkflowSummary } from '@/types';

import { WorkflowListBody } from './WorkflowList';
import { WorkflowRow } from './WorkflowRow';
import {
  EMPTY_WORKFLOW_LIST_FILTER_STATE,
  type WorkflowListPaginationState,
  workflowFilterFromState,
} from '../types';

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
    const markup = renderToStaticMarkup(
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
    const markup = renderToStaticMarkup(
      <table>
        <tbody>
          <WorkflowRow workflow={workflow} />
        </tbody>
      </table>
    );

    expect(markup).toContain('/workflows/workflow-1');
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

function resetPaginationOnFilterChange(
  pagination: WorkflowListPaginationState
): WorkflowListPaginationState {
  void pagination;
  return { cursor: undefined, previousCursors: [] };
}
