import { Skeleton } from '@/components/ui';

type LoadingSkeletonProps = {
  label?: string;
  rows?: number;
};

function LoadingSkeleton({ label = 'Loading timeline', rows = 4 }: LoadingSkeletonProps) {
  return (
    <div aria-label={label} className="space-y-4" role="status">
      <span className="sr-only">{label}</span>
      {Array.from({ length: rows }, (_, index) => (
        <div className="flex gap-4" key={index.toString()}>
          <Skeleton className="mt-1 size-10 rounded-full" />
          <div className="flex-1 space-y-3 rounded-xl border border-[var(--border-default)] p-4">
            <Skeleton className="h-4 w-40" />
            <Skeleton className="h-5 w-2/3" />
            <Skeleton className="h-3 w-full" />
          </div>
        </div>
      ))}
    </div>
  );
}

export { LoadingSkeleton };
