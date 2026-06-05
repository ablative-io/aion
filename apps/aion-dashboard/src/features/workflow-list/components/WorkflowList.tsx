import { useMemo, useState } from 'react';

import { EmptyState } from '@/components/EmptyState';
import { ErrorState } from '@/components/ErrorState';
import { LoadingSkeleton } from '@/components/LoadingSkeleton';
import { Table, TableBody, TableHead, TableHeader, TableRow } from '@/components/ui';
import type { ApiClient } from '@/lib/api';
import type { Namespace, WorkflowFilter } from '@/types';
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
  namespace: Namespace;
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
  const query = useWorkflowQuery(
    namespace,
    filter,
    { cursor: pagination.cursor, limit: normalizedPageLimit },
    { client }
  );

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

  return (
    <section className="space-y-4">
      <FilterBar value={filterState} onChange={handleFilterChange} />
      <WorkflowListBody
        query={query}
        canGoPrevious={pagination.previousCursors.length > 0}
        onNext={handleNextPage}
        onPrevious={handlePreviousPage}
      />
    </section>
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
        message={errorMessage(query.error)}
        actionLabel="Retry"
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

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
