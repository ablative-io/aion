import { Skeleton } from '@/components/ui';

type LoadingSkeletonVariant = 'timeline' | 'list';

type LoadingSkeletonProps = {
  rows?: number;
  label?: string;
  variant?: LoadingSkeletonVariant;
};

function LoadingSkeleton({ rows = 4, label, variant = 'timeline' }: LoadingSkeletonProps) {
  const resolvedLabel = label ?? (variant === 'timeline' ? 'Loading timeline' : 'Loading list');

  return (
    <div aria-label={resolvedLabel} className="space-y-4" role="status">
      {Array.from({ length: rows }, (_, index) =>
        variant === 'list' ? (
          <ListSkeletonRow key={index.toString()} />
        ) : (
          <TimelineSkeletonRow key={index.toString()} />
        )
      )}
    </div>
  );
}

function ListSkeletonRow() {
  return (
    <div
      className="grid gap-3 rounded-xl border border-[var(--border-default)] p-4 md:grid-cols-[1.4fr_1fr_0.7fr_1fr_1fr]"
    >
      <Skeleton className="h-4 w-44" />
      <Skeleton className="h-4 w-32" />
      <Skeleton className="h-5 w-20 rounded-full" />
      <Skeleton className="h-4 w-28" />
      <Skeleton className="h-4 w-24" />
    </div>
  );
}

function TimelineSkeletonRow() {
  return (
    <div className="flex gap-4">
      <Skeleton className="mt-1 size-10 rounded-full" />
      <div className="flex-1 space-y-3 rounded-xl border border-[var(--border-default)] p-4">
        <Skeleton className="h-4 w-40" />
        <Skeleton className="h-5 w-2/3" />
        <Skeleton className="h-3 w-full" />
      </div>
    </div>
  );
}

export type { LoadingSkeletonVariant };
export { LoadingSkeleton };
