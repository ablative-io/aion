import { Skeleton } from '@/components/ui';

export type LoadingSkeletonProps = {
  rows?: number;
  label?: string;
};

export function LoadingSkeleton({ rows = 3, label = 'Loading' }: LoadingSkeletonProps) {
  return (
    <div className="space-y-3" role="status" aria-label={label} aria-busy="true">
      {Array.from({ length: rows }, (_, index) => (
        <div className="grid grid-cols-5 gap-3" key={`skeleton-row-${String(index)}`}>
          <Skeleton className="h-9" />
          <Skeleton className="h-9" />
          <Skeleton className="h-9" />
          <Skeleton className="h-9" />
          <Skeleton className="h-9" />
        </div>
      ))}
    </div>
  );
}
