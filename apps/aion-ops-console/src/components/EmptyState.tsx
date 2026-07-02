import type { ReactNode } from 'react';

type EmptyStateProps = {
  title: string;
  message?: string;
  description?: string;
  icon?: ReactNode;
};

function EmptyState({ title, message, description, icon }: EmptyStateProps) {
  const body = description ?? message;

  return (
    <div className="rounded-xl border border-dashed border-border p-8 text-center">
      {icon === undefined ? null : (
        <div className="mx-auto mb-4 flex size-12 items-center justify-center rounded-full bg-surface-hover text-muted-foreground">
          {icon}
        </div>
      )}
      <h2 className="font-medium text-foreground text-lg">{title}</h2>
      {body === undefined ? null : <p className="mt-2 text-muted-foreground text-sm">{body}</p>}
    </div>
  );
}

export { EmptyState };
