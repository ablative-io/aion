import { Button } from '@/components/ui';

export type ErrorStateProps = {
  title: string;
  message: string;
  actionLabel?: string;
  onRetry?: () => void;
};

export function ErrorState({ title, message, actionLabel = 'Retry', onRetry }: ErrorStateProps) {
  return (
    <div className="rounded-lg border border-[var(--destructive)]/40 p-6">
      <p className="font-medium text-[var(--destructive)]">{title}</p>
      <p className="mt-2 text-sm text-[var(--text-muted)]">{message}</p>
      {onRetry === undefined ? null : (
        <Button className="mt-4" type="button" variant="outline" size="sm" onClick={onRetry}>
          {actionLabel}
        </Button>
      )}
    </div>
  );
}
