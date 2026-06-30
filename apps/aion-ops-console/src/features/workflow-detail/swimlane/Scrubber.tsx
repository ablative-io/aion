import { useMemo } from 'react';

import { Button } from '@/components/ui';
import { cn } from '@/lib/utils';

import type { TimelineEntry } from '../types';
import { scrubSequences } from './scrub';

type ScrubberProps = {
  entries: readonly TimelineEntry[];
  /** Current cut; `null` is live (handle at the far right). */
  scrubSeq: number | null;
  /** Emits the seq the handle maps to, or `null` when released back to live. */
  onScrub: (scrubSeq: number | null) => void;
};

/**
 * Execution Scrubber (PLAN S5, VISION §4.2). A draggable handle over the workflow
 * time axis: dragging to position N reconstructs the recorded prefix at seq N
 * (faithful projection, no engine replay). The handle's POSITION is the source of
 * truth — it indexes the dense list of recorded sequences, so it lands exactly on
 * real stops (no fabricated in-between values). The far-right stop is "live"; the
 * Live button (and dragging fully right) releases the scrub. State is conveyed by
 * position + a single readout, no chrome (VISION §1).
 */
function Scrubber({ entries, scrubSeq, onScrub }: ScrubberProps) {
  const stops = useMemo(() => scrubSequences(entries), [entries]);

  if (stops.length === 0) {
    return null;
  }

  const lastIndex = stops.length - 1;
  // null (live) parks the handle at the far-right stop.
  const activeIndex = scrubSeq === null ? lastIndex : indexForSeq(stops, scrubSeq);
  const isLive = scrubSeq === null || activeIndex === lastIndex;

  function handleChange(rawIndex: number) {
    const clamped = Math.min(lastIndex, Math.max(0, rawIndex));

    if (clamped === lastIndex) {
      onScrub(null);
      return;
    }

    onScrub(stops[clamped] ?? null);
  }

  return (
    <fieldset
      aria-label="Execution scrubber"
      className="flex items-center gap-3 rounded-xl border border-[var(--border-default)] bg-[var(--surface-elevated)] px-4 py-3"
    >
      <span className="shrink-0 text-[var(--text-muted)] text-xs uppercase tracking-wide">
        Scrub
      </span>
      <input
        aria-label="Scrub to recorded sequence"
        aria-valuetext={
          isLive ? 'Live (latest recorded sequence)' : `Sequence ${stops[activeIndex]}`
        }
        className={cn(
          'h-1.5 min-w-0 flex-1 cursor-pointer appearance-none rounded-full bg-[var(--surface-hover)]',
          'accent-[var(--accent-cyan)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--accent-cyan)]'
        )}
        max={lastIndex}
        min={0}
        onChange={(event) => handleChange(Number(event.target.value))}
        step={1}
        type="range"
        value={activeIndex}
      />
      <output
        aria-live="polite"
        className="w-28 shrink-0 text-right text-[var(--text-primary)] text-xs tabular-nums"
      >
        {isLive ? (
          <span className="text-[var(--accent-cyan)]">live · seq {stops[lastIndex]}</span>
        ) : (
          <span>
            seq {stops[activeIndex]}{' '}
            <span className="text-[var(--text-muted)]">of {stops[lastIndex]}</span>
          </span>
        )}
      </output>
      <Button
        aria-pressed={isLive}
        className="h-7 shrink-0 px-3 text-xs"
        disabled={isLive}
        onClick={() => onScrub(null)}
        type="button"
        variant="ghost"
      >
        Live
      </Button>
    </fieldset>
  );
}

/** Map a seq to its handle index, snapping up to the next recorded stop. */
function indexForSeq(stops: readonly number[], seq: number): number {
  const exact = stops.indexOf(seq);

  if (exact !== -1) {
    return exact;
  }

  const next = stops.findIndex((stop) => stop >= seq);

  return next === -1 ? stops.length - 1 : next;
}

export { Scrubber };
