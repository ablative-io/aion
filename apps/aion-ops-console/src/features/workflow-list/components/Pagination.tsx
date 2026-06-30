import { Button } from '@/components/ui';

export type PaginationProps = {
  canGoPrevious: boolean;
  canGoNext: boolean;
  isFetching?: boolean;
  onPrevious: () => void;
  onNext: () => void;
};

export function Pagination({
  canGoPrevious,
  canGoNext,
  isFetching = false,
  onPrevious,
  onNext,
}: PaginationProps) {
  return (
    <nav className="flex items-center justify-end gap-2" aria-label="Workflow pagination">
      <Button
        type="button"
        variant="outline"
        size="sm"
        disabled={!canGoPrevious || isFetching}
        onClick={onPrevious}
      >
        Previous
      </Button>
      <Button
        type="button"
        variant="outline"
        size="sm"
        disabled={!canGoNext || isFetching}
        onClick={onNext}
      >
        Next
      </Button>
    </nav>
  );
}
