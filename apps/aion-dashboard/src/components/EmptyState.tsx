export type EmptyStateProps = {
  title: string;
  description?: string;
};

export function EmptyState({ title, description }: EmptyStateProps) {
  return (
    <div className="rounded-lg border border-dashed border-[var(--border-default)] p-8 text-center">
      <p className="font-medium">{title}</p>
      {description === undefined ? null : (
        <p className="mt-2 text-sm text-[var(--text-muted)]">{description}</p>
      )}
    </div>
  );
}
