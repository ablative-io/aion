import { useMemo, useState } from 'react';

import { EmptyState } from '@/components/EmptyState';
import { ErrorState } from '@/components/ErrorState';
import { LoadingSkeleton } from '@/components/LoadingSkeleton';
import { Button, Table, TableBody, TableHead, TableHeader, TableRow } from '@/components/ui';
import type { ApiClient } from '@/lib/api';
import type { Namespace, WorkflowFilter } from '@/types';
import { useLiveListUpdates } from '../hooks/useLiveListUpdates';
import { useWorkflowQuery } from '../hooks/useWorkflowQuery';
import {
  EMPTY_WORKFLOW_LIST_FILTER_STATE,
  FIRST_WORKFLOW_LIST_PAGE,
  WORKFLOW_PAGE_LIMIT,
  type WorkflowListFilterState,
  workflowFilterFromState,
} from '../types';
import { FilterBar } from './FilterBar';
import { Pagination } from './Pagination';
import { WorkflowRow } from './WorkflowRow';

export type WorkflowListProps = {
  namespace: Namespace | null;
  client?: ApiClient;
  initialFilterState?: WorkflowListFilterState;
  pageLimit?: number;
};

export function WorkflowList({
  namespace,
  client,
  initialFilterState = EMPTY_WORKFLOW_LIST_FILTER_STATE,
  pageLimit = WORKFLOW_PAGE_LIMIT,
}: WorkflowListProps) {
  const [filterState, setFilterState] = useState(initialFilterState);
  const [pagination, setPagination] = useState(FIRST_WORKFLOW_LIST_PAGE);
  const [filter, setFilter] = useState(() => workflowFilterFromState(initialFilterState));
  const normalizedPageLimit = useMemo(() => normalizePageLimit(pageLimit), [pageLimit]);
  const page = useMemo(
    () => ({ cursor: pagination.cursor, limit: normalizedPageLimit }),
    [pagination.cursor, normalizedPageLimit]
  );
  const isFirstPage = pagination.cursor === undefined;
  const query = useWorkflowQuery({
    apiClient: client,
    filter,
    page,
  });
  // Close the dead-wired C4 path: live-patch the React Query cache for this
  // exact page key so rows transition (Running -> Completed) and new workflows
  // appear without a refetch. Off page 1 new rows are surfaced as "new above"
  // rather than mutating the cursor view (R3).
  const live = useLiveListUpdates({ filter, page, isFirstPage });

  function handleFilterChange(nextFilter: WorkflowFilter, state: WorkflowListFilterState) {
    setFilterState(state);
    setFilter(nextFilter);
    setPagination(FIRST_WORKFLOW_LIST_PAGE);
  }

  function handleNextPage() {
    const nextCursor = query.data?.nextCursor;

    if (nextCursor === undefined || nextCursor === null) {
      return;
    }

    setPagination((current) => ({
      cursor: nextCursor,
      previousCursors: [...current.previousCursors, current.cursor ?? ''],
    }));
  }

  function handlePreviousPage() {
    setPagination((current) => {
      const previousCursors = current.previousCursors.slice(0, -1);
      const previousCursor = current.previousCursors.at(-1);

      return {
        cursor: previousCursor === '' ? undefined : previousCursor,
        previousCursors,
      };
    });
  }

  if (namespace === null) {
    return (
      <EmptyState
        description="Select a namespace to scope the workflow query."
        title="No namespace selected"
      />
    );
  }

  function handleShowNewAbove() {
    live.acknowledgeNewAbove();
    setPagination(FIRST_WORKFLOW_LIST_PAGE);
  }

  return (
    <section className="space-y-4">
      <FilterBar value={filterState} onChange={handleFilterChange} />
      <NewAboveBanner count={live.newAboveCount} onShow={handleShowNewAbove} />
      <WorkflowListBody
        query={query}
        canGoPrevious={pagination.previousCursors.length > 0}
        onNext={handleNextPage}
        onPrevious={handlePreviousPage}
      />
    </section>
  );
}

type NewAboveBannerProps = {
  count: number;
  onShow: () => void;
};

export function NewAboveBanner({ count, onShow }: NewAboveBannerProps) {
  if (count <= 0) {
    return null;
  }

  const label =
    count === 1 ? '1 new workflow started above' : `${count} new workflows started above`;

  return (
    <div className="flex items-center justify-between rounded-md border border-[var(--border-default)] bg-[var(--surface-hover)] px-3 py-2 text-sm">
      <span className="text-[var(--text-muted)]">{label}</span>
      <Button type="button" variant="outline" size="sm" onClick={onShow}>
        Jump to top
      </Button>
    </div>
  );
}

type WorkflowListBodyProps = {
  query: ReturnType<typeof useWorkflowQuery>;
  canGoPrevious: boolean;
  onPrevious: () => void;
  onNext: () => void;
};

export function WorkflowListBody({
  query,
  canGoPrevious,
  onPrevious,
  onNext,
}: WorkflowListBodyProps) {
  if (query.isPending) {
    return <LoadingSkeleton rows={6} label="Loading workflow summaries" />;
  }

  if (query.isError) {
    return (
      <ErrorState
        title="Could not load workflows"
        error={query.error}
        onRetry={() => void query.refetch()}
      />
    );
  }

  const page = query.data;

  if (page === undefined) {
    return <LoadingSkeleton rows={6} label="Loading workflow summaries" />;
  }

  if (page.items.length === 0) {
    return <EmptyState title="No workflows match this filter" />;
  }

  return (
    <div className="space-y-3">
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Workflow ID</TableHead>
            <TableHead>Type</TableHead>
            <TableHead>Status</TableHead>
            <TableHead>Started</TableHead>
            <TableHead>Ended</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {page.items.map((workflow) => (
            <WorkflowRow key={workflow.workflow_id} workflow={workflow} />
          ))}
        </TableBody>
      </Table>
      <Pagination
        canGoPrevious={canGoPrevious}
        canGoNext={page.hasMore && page.nextCursor !== null && page.nextCursor.length > 0}
        isFetching={query.isFetching}
        onPrevious={onPrevious}
        onNext={onNext}
      />
    </div>
  );
}

function normalizePageLimit(limit: number): number {
  if (!Number.isFinite(limit)) {
    return WORKFLOW_PAGE_LIMIT;
  }

  return Math.max(1, Math.floor(limit));
}
