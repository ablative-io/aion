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
    <div className="rounded-xl border border-dashed border-[var(--border-default)] p-8 text-center">
      {icon === undefined ? null : (
        <div className="mx-auto mb-4 flex size-12 items-center justify-center rounded-full bg-[var(--surface-hover)] text-[var(--text-muted)]">
          {icon}
        </div>
      )}
      <h2 className="font-medium text-[var(--text-primary)] text-lg">{title}</h2>
      {body === undefined ? null : <p className="mt-2 text-[var(--text-muted)] text-sm">{body}</p>}
    </div>
  );
}

export { EmptyState };
