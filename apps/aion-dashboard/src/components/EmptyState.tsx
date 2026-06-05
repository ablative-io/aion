type EmptyStateProps = {
  title: string;
  description?: string;
};

function EmptyState({ title, description }: EmptyStateProps) {
  return (
    <div className="rounded-xl border border-dashed border-[var(--border-default)] p-8 text-center">
      <h2 className="font-medium text-[var(--text-primary)] text-lg">{title}</h2>
      {description === undefined ? null : (
        <p className="mt-2 text-[var(--text-muted)] text-sm">{description}</p>
      )}
    </div>
  );
}

export { EmptyState };
