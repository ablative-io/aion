import { cn } from '@/lib/utils';

import type { BarStatus, SwimlaneBar } from './laneLayout';
import type { PositionedBar } from './timeLayout';

type LaneBarProps = {
  positioned: PositionedBar;
  selected: boolean;
  /** Select this bar and preserve its viewport x-origin for the detail morph. */
  onSelect: (bar: SwimlaneBar, originX: number) => void;
};

/** A status/shape encoded bar already resolved onto the active shared axis. */
function LaneBar({ positioned, selected, onSelect }: LaneBarProps) {
  const { bar, left, width, marker, attemptOffsets } = positioned;

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
      data-marker={marker ? 'true' : undefined}
      onClick={(event) => {
        const rect = event.currentTarget.getBoundingClientRect();
        onSelect(bar, rect.left + rect.width / 2);
      }}
      style={{ left, width }}
      title={bar.label}
      type="button"
    >
      {marker ? null : <AttemptSegments bar={bar} offsets={attemptOffsets} />}
      <span className="relative z-10 truncate px-2 text-[11px] text-foreground">
        {marker ? '' : bar.label}
        {bar.childWorkflowId ? <span className="ml-1 opacity-70">↳</span> : null}
      </span>
    </button>
  );
}

function AttemptSegments({ bar, offsets }: { bar: SwimlaneBar; offsets: readonly number[] }) {
  if (offsets.length === 0) {
    return null;
  }

  return (
    <>
      {offsets.map((offset, index) => (
        <span
          aria-hidden="true"
          className="absolute top-0 bottom-0 w-px bg-border"
          data-attempt-divider="true"
          key={`${bar.id}:attempt:${bar.attemptSequences[index] ?? index}`}
          style={{ left: Math.max(1, offset) }}
        />
      ))}
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
