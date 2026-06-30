import { Badge } from '@/components/ui';
import type { WorkflowStatus } from '@/types';

export type StatusBadgeProps = {
  status: WorkflowStatus;
};

export type StatusBadgeMetadata = {
  label: string;
  className: string;
};

export const STATUS_BADGE_METADATA: Record<WorkflowStatus, StatusBadgeMetadata> = {
  Running: {
    label: 'Running',
    className: 'border-sky-400/30 bg-sky-500/15 text-sky-300',
  },
  Completed: {
    label: 'Completed',
    className: 'border-emerald-400/30 bg-emerald-500/15 text-emerald-300',
  },
  Failed: {
    label: 'Failed',
    className: 'border-red-400/30 bg-red-500/15 text-red-300',
  },
  Cancelled: {
    label: 'Cancelled',
    className: 'border-zinc-400/30 bg-zinc-500/15 text-zinc-300',
  },
  TimedOut: {
    label: 'Timed out',
    className: 'border-amber-400/30 bg-amber-500/15 text-amber-300',
  },
  ContinuedAsNew: {
    label: 'Continued as new',
    className: 'border-violet-400/30 bg-violet-500/15 text-violet-300',
  },
};

export function StatusBadge({ status }: StatusBadgeProps) {
  const metadata = STATUS_BADGE_METADATA[status];

  return (
    <Badge className={metadata.className} data-status={status} variant="outline">
      {metadata.label}
    </Badge>
  );
}
