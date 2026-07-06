import { cn } from '@/lib/utils';

import type { BarStatus, SwimlaneBar } from './laneLayout';

type LaneBarProps = {
  bar: SwimlaneBar;
  /** Column width in px (one dense rank). */
  columnWidth: number;
  selected: boolean;
  /**
   * Select this bar. `originX` is the bar's horizontal centre in viewport px, so
   * the bottom-docked detail sheet can morph out of the clicked bar's rect.
   */
  onSelect: (bar: SwimlaneBar, originX: number) => void;
};

/**
 * One bar in a lane. Status is conveyed by COLOR + SHAPE + POSITION only (no
 * gradients/glass — VISION §1). An activity bar with retries is segmented at each
 * `ActivityFailed.attempt`. A child bar carries a drill-link affordance. Markers
 * (lifecycle/signal/generic single-seq points) render as a small node, spans as a
 * bar from startRank→endRank.
 */
function LaneBar({ bar, columnWidth, selected, onSelect }: LaneBarProps) {
  const left = bar.startRank * columnWidth;
  const span = Math.max(1, bar.endRank - bar.startRank + 1);
  const width = bar.isMarker ? Math.min(columnWidth, 14) : span * columnWidth - 4;

  return (
    <button
      aria-current={selected ? 'true' : undefined}
      aria-label={`${bar.label} (seq ${bar.sequence})`}
      className={cn(
        'absolute top-1 flex h-7 items-center overflow-hidden rounded-md border text-left',
        'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
        STATUS_CLASSES[bar.status],
        selected && 'ring-2 ring-primary'
      )}
      data-bar-status={bar.status}
      data-marker={bar.isMarker ? 'true' : undefined}
      onClick={(event) => {
        const rect = event.currentTarget.getBoundingClientRect();
        onSelect(bar, rect.left + rect.width / 2);
      }}
      style={{ left, width }}
      title={bar.label}
      type="button"
    >
      {bar.isMarker ? null : <AttemptSegments bar={bar} columnWidth={columnWidth} />}
      <span className="relative z-10 truncate px-2 text-[11px] text-foreground">
        {bar.isMarker ? '' : bar.label}
        {bar.childWorkflowId ? <span className="ml-1 opacity-70">↳</span> : null}
      </span>
    </button>
  );
}

/**
 * Render attempt boundaries inside an activity bar as thin dividers so a retried
 * activity reads as segmented attempts (one-based `ActivityFailed.attempt`).
 */
function AttemptSegments({ bar, columnWidth }: { bar: SwimlaneBar; columnWidth: number }) {
  if (bar.attemptRanks.length === 0) {
    return null;
  }

  return (
    <>
      {bar.attemptRanks.map((rank) => {
        const offset = (rank - bar.startRank) * columnWidth;

        return (
          <span
            aria-hidden="true"
            className="absolute top-0 bottom-0 w-px bg-border"
            data-attempt-divider="true"
            key={`${bar.id}:attempt:${rank}`}
            style={{ left: Math.max(1, offset) }}
          />
        );
      })}
    </>
  );
}

const STATUS_CLASSES: Record<BarStatus, string> = {
  running: 'border-live/40 bg-live/20',
  completed: 'border-success/40 bg-success/20',
  failed: 'border-danger/50 bg-danger/25',
  cancelled: 'border-warning/40 bg-warning/20',
  'timed-out': 'border-warning/50 bg-warning/25',
  fired: 'border-live/40 bg-live-glow',
  marker: 'border-border bg-surface-hover',
};

export { LaneBar };
