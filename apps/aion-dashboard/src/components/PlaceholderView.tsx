import { EmptyState } from '@/components/EmptyState';

export type PlaceholderViewProps = {
  /** Name of the view this placeholder stands in for (e.g. "Event search"). */
  title: string;
  /** What the finished view will do, so the empty surface is self-describing. */
  description: string;
};

/**
 * Honest stand-in for a Phase-1 view whose component lands in a later slice.
 * Renders a self-describing surface so the route is navigable and the shell is
 * complete; it never fabricates data. Later slices replace the feature root that
 * the route lazy-loads, leaving this used only as a fallback.
 */
export function PlaceholderView({ title, description }: PlaceholderViewProps) {
  return (
    <section aria-label={title} className="py-4">
      <EmptyState description={description} title={title} />
    </section>
  );
}
