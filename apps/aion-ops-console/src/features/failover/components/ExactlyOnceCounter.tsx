export type ExactlyOnceCounterProps = {
  /** Distinct completed activities (seq-deduped — the exactly-once proof). */
  completed: number;
  /** Expected fan-out width; null when not yet derived. */
  arity: number | null;
  /**
   * True when the live stream is not confirmed connected, so the value may be
   * behind. Surfaced as a visible stale badge — never blanked, never fake-live.
   */
  stale: boolean;
  /**
   * True when the raw distinct count exceeds arity — a contract violation that
   * MUST be shown loudly (a "5/4" on stage), never silently clamped away.
   */
  overCount: boolean;
};

/**
 * The headline. The largest type on screen — this number is the load-bearing proof
 * that work survived the kill exactly once. Degradation (§1.1): a lost stream shows
 * a stale badge over the last-known value (never blank); an over-count shows a hard
 * violation badge rather than hiding it.
 */
export function ExactlyOnceCounter({
  completed,
  arity,
  stale,
  overCount,
}: ExactlyOnceCounterProps) {
  return (
    <section className="flex flex-col items-center gap-3 rounded-md border border-[var(--border-default)] bg-[var(--card)] py-10">
      <p className="font-medium text-[0.7rem] text-[var(--text-muted)] uppercase tracking-[0.35em]">
        exactly once
      </p>

      <div className="flex items-baseline gap-4 font-mono">
        <span aria-hidden="true" style={{ color: 'var(--accent-cyan)' }}>
          ▶
        </span>
        <span
          className="font-bold text-7xl tracking-tight tabular-nums sm:text-8xl"
          style={{ color: overCount ? 'var(--destructive)' : 'var(--text-primary)' }}
        >
          {completed} / {arity ?? '?'}
        </span>
      </div>

      <p className="text-[var(--text-muted)] text-sm">
        completed activities · zero duplicates across the kill
      </p>

      <div className="flex gap-2">
        {stale ? (
          <span className="rounded border border-amber-500/40 bg-amber-500/10 px-2 py-0.5 text-amber-500 text-xs">
            stale · stream not confirmed
          </span>
        ) : null}
        {overCount ? (
          <span className="rounded border border-[var(--destructive)]/40 bg-[var(--destructive)]/10 px-2 py-0.5 text-[var(--destructive)] text-xs">
            OVER-COUNT — contract violated
          </span>
        ) : null}
      </div>
    </section>
  );
}
