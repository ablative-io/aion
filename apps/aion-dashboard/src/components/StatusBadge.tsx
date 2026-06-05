import { Badge } from '@/components/ui';
import type { WorkflowStatus } from '@/types';

export type StatusBadgeProps = {
  status: WorkflowStatus;
};

export function StatusBadge({ status }: StatusBadgeProps) {
  return <Badge variant="secondary">{status}</Badge>;
}
