export type FanOutBarProps = {
  /** Fan-out workflow label (e.g. collect_four). */
  label: string;
  /** Distinct completed activities (seq-deduped). */
  completed: number;
  /** Expected fan-out width; null when not yet derived. */
  arity: number | null;
  /** True when the progress stream is not confirmed connected (poll/degraded). */
  stale: boolean;
};

const SEGMENTS = 8;

// Stable per-position identities for the fixed-width segment bar. The bar always has
// exactly SEGMENTS slots, so each slot has a fixed identity independent of render —
// keying on these (rather than the map index) keeps React reconciliation stable.
const SEGMENT_IDS: readonly string[] = Array.from(
  { length: SEGMENTS },
  (_, segment) => `fan-out-segment-${segment}`
);

/**
 * Fan-out progress bar: `done / arity`. Degradation (§1.1): when the progress
 * stream is lost the bar greys and shows a "progress stream lost" badge rather than
 * freezing on a stale-looking solid bar; an unknown arity renders the count alone
 * rather than fabricating a denominator.
 */
export function FanOutBar({ label, completed, arity, stale }: FanOutBarProps) {
  const filled =
    arity !== null && arity > 0
      ? Math.min(SEGMENTS, Math.round((completed / arity) * SEGMENTS))
      : 0;

  return (
    <div className="flex flex-col gap-2 rounded-md border border-border bg-card p-4">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <span className="font-medium text-[0.7rem] text-muted-foreground uppercase tracking-[0.2em]">
            fan-out
          </span>
          <span className="font-mono text-secondary-foreground text-sm">{label}</span>
        </div>
        {stale ? (
          <span className="rounded border border-destructive/40 bg-destructive/10 px-2 py-0.5 text-destructive text-xs">
            progress stream lost
          </span>
        ) : null}
      </div>

      <div className="flex items-center gap-3">
        <div className="flex flex-1 gap-1" data-stale={stale}>
          {SEGMENT_IDS.map((id, segment) => (
            <span
              className="h-3 flex-1 rounded-sm"
              key={id}
              style={{
                backgroundColor:
                  segment < filled
                    ? stale
                      ? 'var(--text-muted)'
                      : 'var(--status-live)'
                    : 'var(--accent)',
              }}
            />
          ))}
        </div>
        <span className="font-mono text-secondary-foreground text-sm">
          {completed} / {arity ?? '?'} tasks
        </span>
      </div>
    </div>
  );
}
