import type { WorkflowFilter, WorkflowStatus } from '@/types';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui';

import {
  type WorkflowListFilterState,
  WORKFLOW_STATUS_OPTIONS,
  workflowFilterFromState,
} from '../types';

const ALL_STATUSES_VALUE = 'all';

export type FilterBarProps = {
  value: WorkflowListFilterState;
  onChange: (filter: WorkflowFilter, state: WorkflowListFilterState) => void;
};

export function FilterBar({ value, onChange }: FilterBarProps) {
  function update(next: WorkflowListFilterState) {
    onChange(workflowFilterFromState(next), next);
  }

  return (
    <div className="grid gap-3 rounded-lg border border-[var(--border-default)] p-4 md:grid-cols-4">
      <label className="flex flex-col gap-2 text-sm font-medium">
        Workflow type
        <input
          className="h-9 rounded-md border border-input bg-transparent px-3 text-sm outline-none"
          placeholder="All workflow types"
          type="text"
          value={value.workflowType}
          onChange={(event) => update({ ...value, workflowType: event.currentTarget.value })}
        />
      </label>
      <label className="flex flex-col gap-2 text-sm font-medium">
        Status
        <Select
          value={value.status ?? ALL_STATUSES_VALUE}
          onValueChange={(status) => update({ ...value, status: statusFromSelectValue(status) })}
        >
          <SelectTrigger className="w-full">
            <SelectValue placeholder="All statuses" />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={ALL_STATUSES_VALUE}>All statuses</SelectItem>
            {WORKFLOW_STATUS_OPTIONS.map((status) => (
              <SelectItem key={status} value={status}>
                {status}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </label>
      <label className="flex flex-col gap-2 text-sm font-medium">
        Started after
        <input
          className="h-9 rounded-md border border-input bg-transparent px-3 text-sm outline-none"
          type="datetime-local"
          value={value.startedAfter}
          onChange={(event) => update({ ...value, startedAfter: event.currentTarget.value })}
        />
      </label>
      <label className="flex flex-col gap-2 text-sm font-medium">
        Started before
        <input
          className="h-9 rounded-md border border-input bg-transparent px-3 text-sm outline-none"
          type="datetime-local"
          value={value.startedBefore}
          onChange={(event) => update({ ...value, startedBefore: event.currentTarget.value })}
        />
      </label>
    </div>
  );
}

function statusFromSelectValue(value: string): WorkflowStatus | null {
  if (value === ALL_STATUSES_VALUE) {
    return null;
  }

  if (WORKFLOW_STATUS_OPTIONS.some((status) => status === value)) {
    return value as WorkflowStatus;
  }

  return null;
}
