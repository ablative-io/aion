import type { AdoptionMoment } from '../hooks/useAdoptionMoment';

export type AdoptionStripProps = {
  /** Label of the seeded shard-0 owner (the kill target). */
  oldOwnerLabel: string;
  /** Label of the survivor that adopted the shard; null until known. */
  newOwnerLabel: string | null;
  /** Inferred adoption moment from the liveness-dark + tally-resume derivation. */
  moment: AdoptionMoment;
};

function elapsedLabel(elapsedMs: number | null): string {
  if (elapsedMs === null) {
    return '';
  }

  return `+${(elapsedMs / 1000).toFixed(1)}s`;
}

/**
 * The adoption story as a single strip: old owner ──✕──▶ new owner, plus the
 * inferred instant. Honest degradation (§1.1): the adoption beat has no structured
 * wire event, so when it is only inferable we say so ("adoption inferred — instant
 * approximate") rather than presenting a fabricated-precise timestamp; before
 * adoption it reads "awaiting adoption" rather than fabricating a moment.
 */
export function AdoptionStrip({ oldOwnerLabel, newOwnerLabel, moment }: AdoptionStripProps) {
  return (
    <div className="flex flex-col gap-2 rounded-md border border-[var(--border-default)] bg-[var(--card)] p-4 sm:flex-row sm:items-center sm:justify-between">
      <div className="flex items-center gap-3">
        <span className="font-medium text-[0.7rem] text-[var(--text-muted)] uppercase tracking-[0.2em]">
          shard 0 ownership
        </span>
        <span className="flex items-center gap-2 font-mono text-sm">
          <span className="text-[var(--text-secondary)]">{oldOwnerLabel}</span>
          <span aria-hidden="true" style={{ color: 'var(--destructive)' }}>
            ──✕──▶
          </span>
          <span
            style={{ color: newOwnerLabel === null ? 'var(--text-muted)' : 'var(--accent-cyan)' }}
          >
            {newOwnerLabel ?? 'awaiting adoption'}
          </span>
        </span>
      </div>

      <div className="font-mono text-sm">
        {moment.adopted ? (
          <span style={{ color: 'var(--accent-cyan)' }}>
            adopted @ {elapsedLabel(moment.elapsedMs)}
            <span className="ml-2 text-[var(--text-muted)] text-xs">
              {moment.degraded ? 'inferred · approximate' : 'instant'}
            </span>
          </span>
        ) : (
          <span className="text-[var(--text-muted)]">adoption inferred — instant unavailable</span>
        )}
      </div>
    </div>
  );
}
