import { generatePath, Link } from 'react-router';
import { StatusBadge } from '@/components/StatusBadge';
import { TableCell, TableRow } from '@/components/ui';
import type { WorkflowSummary } from '@/types';

export type WorkflowRowProps = {
  workflow: WorkflowSummary;
};

export function WorkflowRow({ workflow }: WorkflowRowProps) {
  return (
    <TableRow>
      <TableCell className="font-mono text-xs">
        <Link
          className="text-[var(--accent-cyan)] underline-offset-4 hover:underline"
          to={generatePath('/workflows/:id', { id: workflow.workflow_id })}
        >
          {workflow.workflow_id}
        </Link>
      </TableCell>
      <TableCell>{workflow.workflow_type}</TableCell>
      <TableCell>
        <StatusBadge status={workflow.status} />
      </TableCell>
      <TableCell>{formatWorkflowTimestamp(workflow.started_at)}</TableCell>
      <TableCell>{formatWorkflowTimestamp(workflow.ended_at)}</TableCell>
    </TableRow>
  );
}

function formatWorkflowTimestamp(value: string | null): string {
  if (value === null || value.length === 0) {
    return '—';
  }

  return value;
}
