import { Skeleton } from '@/components/ui';

export type LoadingSkeletonVariant = 'timeline' | 'list';

type LoadingSkeletonProps = {
  label?: string;
  rows?: number;
  variant?: LoadingSkeletonVariant;
};

function LoadingSkeleton({
  label = 'Loading timeline',
  rows = 4,
  variant = 'timeline',
}: LoadingSkeletonProps) {
  return (
    <div aria-label={label} className="space-y-4" data-variant={variant} role="status">
      <span className="sr-only">{label}</span>
      {Array.from({ length: rows }, (_, index) =>
        variant === 'list' ? (
          <ListRowSkeleton key={index.toString()} />
        ) : (
          <TimelineRowSkeleton key={index.toString()} />
        )
      )}
    </div>
  );
}

function TimelineRowSkeleton() {
  return (
    <div className="flex gap-4">
      <Skeleton className="mt-1 size-10 rounded-full" />
      <div className="flex-1 space-y-3 rounded-xl border border-border p-4">
        <Skeleton className="h-4 w-40" />
        <Skeleton className="h-5 w-2/3" />
        <Skeleton className="h-3 w-full" />
      </div>
    </div>
  );
}

function ListRowSkeleton() {
  return (
    <div className="flex items-center gap-4 rounded-lg border border-border p-3">
      <Skeleton className="h-5 w-1/3" />
      <Skeleton className="h-5 w-20" />
      <Skeleton className="ml-auto h-4 w-24" />
    </div>
  );
}

export { LoadingSkeleton };
