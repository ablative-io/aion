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
    className: 'border-live/30 bg-live-glow text-live',
  },
  Completed: {
    label: 'Completed',
    className: 'border-success/30 bg-success-glow text-success',
  },
  Failed: {
    label: 'Failed',
    className: 'border-danger/30 bg-danger-glow text-danger',
  },
  Cancelled: {
    label: 'Cancelled',
    className: 'border-border bg-surface-hover text-muted-foreground',
  },
  TimedOut: {
    label: 'Timed out',
    className: 'border-warning/30 bg-warning-glow text-warning',
  },
  ContinuedAsNew: {
    label: 'Continued as new',
    className: 'border-special/30 bg-special-glow text-special',
  },
  Paused: {
    label: 'Paused',
    className: 'border-warning/30 bg-warning-glow text-warning',
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
