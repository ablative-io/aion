import { Button } from '@/components/ui';

type ErrorStateProps = {
  title: string;
  error?: unknown;
  message?: string | undefined;
  actionLabel?: string | undefined;
  onRetry?: (() => void) | undefined;
};

function ErrorState({ title, error, message, actionLabel = 'Retry', onRetry }: ErrorStateProps) {
  return (
    <div className="rounded-xl border border-destructive/40 bg-destructive/5 p-6">
      <div className="space-y-2">
        <h2 className="font-medium text-foreground text-lg">{title}</h2>
        <p className="text-muted-foreground text-sm">{message ?? errorMessage(error)}</p>
      </div>
      {onRetry === undefined ? null : (
        <Button className="mt-4" onClick={onRetry} size="sm" type="button" variant="outline">
          {actionLabel}
        </Button>
      )}
    </div>
  );
}

function errorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }

  if (typeof error === 'string') {
    return error;
  }

  return 'An unknown error occurred.';
}

export { ErrorState, errorMessage };
