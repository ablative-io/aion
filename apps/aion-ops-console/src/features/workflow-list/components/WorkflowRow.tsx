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
          className="text-primary underline-offset-4 hover:underline"
          to={generatePath('/workflows/:id', { id: workflow.workflow_id })}
        >
          {workflow.workflow_id}
        </Link>
      </TableCell>
      <TableCell>{workflow.workflow_type}</TableCell>
      <TableCell>
        <div className="flex flex-col gap-1">
          <StatusBadge status={workflow.status} />
          <FailureContext failedStep={workflow.failed_step} reason={workflow.failure_reason} />
        </div>
      </TableCell>
      <TableCell>{formatWorkflowTimestamp(workflow.started_at)}</TableCell>
      <TableCell>{formatWorkflowTimestamp(workflow.ended_at)}</TableCell>
    </TableRow>
  );
}

/**
 * The failure attribution for a failed row: the step that failed and the terminal
 * reason. Rendered ONLY when the server populated them (a failed row) — a
 * healthy/running/completed row carries `null`/`undefined` for both and shows
 * nothing, so no empty failure text appears under a non-failed status.
 */
function FailureContext({
  failedStep,
  reason,
}: {
  failedStep: string | null | undefined;
  reason: string | null | undefined;
}) {
  const hasStep = failedStep !== null && failedStep !== undefined && failedStep.length > 0;
  const hasReason = reason !== null && reason !== undefined && reason.length > 0;
  if (!hasStep && !hasReason) {
    return null;
  }

  return (
    <div className="flex flex-col gap-0.5 text-muted-foreground text-xs">
      {hasStep ? (
        <span>
          Failed at <span className="font-mono text-secondary-foreground">{failedStep}</span>
        </span>
      ) : null}
      {hasReason ? <span className="line-clamp-2 max-w-xs">{reason}</span> : null}
    </div>
  );
}

function formatWorkflowTimestamp(value: string | null | undefined): string {
  if (value === null || value === undefined || value.length === 0) {
    return '—';
  }

  return value;
}
