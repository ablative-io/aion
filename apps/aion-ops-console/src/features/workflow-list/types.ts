import type { WorkflowFilter, WorkflowStatus } from '@/types';

export const WORKFLOW_PAGE_LIMIT = 50;

export const WORKFLOW_STATUS_OPTIONS = [
  'Running',
  'Completed',
  'Failed',
  'Cancelled',
  'TimedOut',
] as const satisfies readonly WorkflowStatus[];

export type WorkflowListFilterState = {
  workflowType: string;
  status: WorkflowStatus | null;
  startedAfter: string;
  startedBefore: string;
};

export type WorkflowListPaginationState = {
  cursor: string | undefined;
  previousCursors: string[];
};

export const EMPTY_WORKFLOW_LIST_FILTER_STATE: WorkflowListFilterState = {
  workflowType: '',
  status: null,
  startedAfter: '',
  startedBefore: '',
};

export const FIRST_WORKFLOW_LIST_PAGE: WorkflowListPaginationState = {
  cursor: undefined,
  previousCursors: [],
};

export function workflowFilterFromState(state: WorkflowListFilterState): WorkflowFilter {
  return {
    workflow_type: nullableTrimmedValue(state.workflowType),
    status: state.status,
    started_after: nullableTrimmedValue(state.startedAfter),
    started_before: nullableTrimmedValue(state.startedBefore),
    parent: null,
  };
}

function nullableTrimmedValue(value: string): string | null {
  const trimmed = value.trim();
  return trimmed.length === 0 ? null : trimmed;
}
