import { Button } from '@/components/ui';
import { cn } from '@/lib/utils';

import { snapGlobalRank } from './scrub';
import type { AxisLayout, AxisMode } from './timeLayout';

type ScrubberProps = {
  axis: AxisLayout | null;
  mode: AxisMode;
  /** Timestamp in time mode, global rank in stepped mode; null is live. */
  scrubPosition: number | null;
  onScrub: (position: number | null) => void;
};

/** Continuous in time mode and rank-snapped over the visible union in stepped mode. */
function Scrubber({ axis, mode, scrubPosition, onScrub }: ScrubberProps) {
  if (axis === null || axis.events.length === 0) {
    return null;
  }

  const minimum = mode === 'time' ? axis.bounds.t0 : 0;
  const maximum = mode === 'time' ? axis.bounds.tEnd : axis.events.length - 1;
  const active = scrubPosition === null ? maximum : clampPosition(scrubPosition, minimum, maximum);
  const isLive = scrubPosition === null || active === maximum;

  function handleChange(raw: number) {
    const next =
      mode === 'stepped'
        ? snapGlobalRank(raw, axis?.events.length ?? 0)
        : clampPosition(raw, minimum, maximum);
    onScrub(next === maximum ? null : next);
  }

  return (
    <fieldset
      aria-label="Execution scrubber"
      className="flex items-center gap-3 border-border border-t bg-surface-base px-4 py-3"
    >
      <span className="shrink-0 text-muted-foreground text-xs uppercase tracking-wide">Scrub</span>
      <input
        aria-label={mode === 'time' ? 'Scrub to recorded time' : 'Scrub to global event rank'}
        aria-valuetext={isLive ? 'Live' : scrubReadout(mode, active, axis)}
        className={cn(
          'h-1.5 min-w-0 flex-1 cursor-pointer appearance-none rounded-full bg-surface-hover',
          'accent-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring'
        )}
        max={maximum}
        min={minimum}
        onChange={(event) => handleChange(Number(event.target.value))}
        step={mode === 'time' ? 1 : 1}
        type="range"
        value={active}
      />
      <output
        aria-live="polite"
        className="w-44 shrink-0 text-right text-foreground text-xs tabular-nums"
      >
        {isLive ? <span className="text-live">live</span> : scrubReadout(mode, active, axis)}
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

function clampPosition(position: number, minimum: number, maximum: number): number {
  return Math.min(maximum, Math.max(minimum, position));
}

function scrubReadout(mode: AxisMode, position: number, axis: AxisLayout): string {
  if (mode === 'time') {
    return new Date(position).toISOString();
  }
  const rank = snapGlobalRank(position, axis.events.length);
  const event = axis.events[rank];
  return event === undefined
    ? `event ${rank + 1}`
    : `event ${rank + 1} · ${event.workflowId} seq ${event.sequence}`;
}

export { Scrubber };
