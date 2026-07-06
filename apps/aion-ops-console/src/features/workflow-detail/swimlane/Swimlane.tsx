import { useMemo, useState } from 'react';

import { EmptyState } from '@/components/EmptyState';
import { EventIcon, type EventIconKind } from '@/components/EventIcon';
import { cn } from '@/lib/utils';

import type { TimelineEntry } from '../types';
import { LaneBar } from './LaneBar';
import { layoutSwimlane, type SwimlaneBar, type SwimlaneLane } from './laneLayout';
import { prefixUpTo } from './scrub';

const COLUMN_WIDTH = 80;
const LANE_LABEL_WIDTH = 168;
const LANE_HEIGHT = 36;

type SwimlaneProps = {
  entries: readonly TimelineEntry[];
  selectedSequence: number | null;
  /**
   * Select a bar by its seq. `origin.x` is the clicked bar's horizontal centre in
   * viewport px so the bottom-docked detail sheet can morph out of that bar.
   */
  onSelect: (sequence: number, origin?: { x: number }) => void;
  /**
   * When set (S5 scrubber), the swimlane renders only the recorded prefix at/below
   * this seq — later bars disappear and re-derived statuses reflect that point in
   * history. `null` is live (the full timeline). See `scrub.ts`.
   */
  scrubSeq?: number | null;
};

/**
 * Partial-order swimlane (VISION §4.1). Renders the workflow as concurrent
 * horizontal lanes keyed on `seq` (not a linear list): a lifecycle lane, one lane
 * per activity / timer / child, plus signal + other lanes. Bars are positioned by
 * dense-seq-rank so concurrent work overlaps across lanes. Selecting a bar lifts its
 * seq + x-origin so the bottom-docked detail sheet morphs out of it. Lanes
 * collapse/expand. This view
 * consumes the S3 `projectTimeline` output ONLY (no event re-parsing, no fabricated
 * data) — it grows in place as live events extend the projected entries.
 */
function Swimlane({ entries, selectedSequence, onSelect, scrubSeq = null }: SwimlaneProps) {
  const layout = useMemo(() => layoutSwimlane(prefixUpTo(entries, scrubSeq)), [entries, scrubSeq]);
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(() => new Set());

  if (layout.lanes.length === 0) {
    return (
      <EmptyState description="No correlated events to lay out yet." title="Nothing to visualize" />
    );
  }

  const trackWidth = Math.max(COLUMN_WIDTH, layout.rankCount * COLUMN_WIDTH);

  function toggleLane(id: string) {
    setCollapsed((current) => {
      const next = new Set(current);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  }

  function handleSelect(bar: SwimlaneBar, originX: number) {
    onSelect(bar.sequence, { x: originX });
  }

  return (
    <section
      aria-label="Workflow swimlane"
      className="overflow-x-auto rounded-xl border border-border bg-surface-elevated"
    >
      <div style={{ minWidth: LANE_LABEL_WIDTH + trackWidth }}>
        <SeqAxis
          labelWidth={LANE_LABEL_WIDTH}
          sequences={layout.sequences}
          trackWidth={trackWidth}
        />
        <ul aria-label="Lanes" className="divide-y divide-border">
          {layout.lanes.map((lane) => (
            <LaneRow
              collapsed={collapsed.has(lane.id)}
              key={lane.id}
              lane={lane}
              onSelectBar={handleSelect}
              onToggle={() => toggleLane(lane.id)}
              selectedSequence={selectedSequence}
              trackWidth={trackWidth}
            />
          ))}
        </ul>
      </div>
    </section>
  );
}

function SeqAxis({
  sequences,
  trackWidth,
  labelWidth,
}: {
  sequences: number[];
  trackWidth: number;
  labelWidth: number;
}) {
  return (
    <div className="flex border-border border-b bg-surface-base">
      <div
        className="shrink-0 px-3 py-1 text-muted-foreground text-xs uppercase tracking-wide"
        style={{ width: labelWidth }}
      >
        Lane
      </div>
      <div className="relative" style={{ width: trackWidth }}>
        {sequences.map((seq, rank) => (
          <span
            className="absolute top-1 text-[10px] text-muted-foreground tabular-nums"
            key={`axis:${seq}`}
            style={{ left: rank * COLUMN_WIDTH + 4 }}
          >
            {seq}
          </span>
        ))}
      </div>
    </div>
  );
}

function LaneRow({
  lane,
  collapsed,
  trackWidth,
  selectedSequence,
  onToggle,
  onSelectBar,
}: {
  lane: SwimlaneLane;
  collapsed: boolean;
  trackWidth: number;
  selectedSequence: number | null;
  onToggle: () => void;
  onSelectBar: (bar: SwimlaneBar, originX: number) => void;
}) {
  return (
    <li className="flex items-stretch">
      <button
        aria-expanded={!collapsed}
        className="flex w-[168px] shrink-0 items-center gap-2 px-3 py-2 text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        onClick={onToggle}
        type="button"
      >
        <EventIcon kind={lane.kind as EventIconKind} />
        <span className="min-w-0 flex-1">
          <span className="block truncate text-foreground text-xs">{lane.label}</span>
          <span className="block text-[10px] text-muted-foreground">
            {lane.bars.length} {lane.bars.length === 1 ? 'event' : 'events'}
            {collapsed ? ' · collapsed' : ''}
          </span>
        </span>
      </button>
      <div
        className={cn('relative', collapsed && 'opacity-40')}
        style={{ width: trackWidth, height: collapsed ? LANE_HEIGHT / 2 : LANE_HEIGHT }}
      >
        {collapsed
          ? null
          : lane.bars.map((bar) => (
              <LaneBar
                bar={bar}
                columnWidth={COLUMN_WIDTH}
                key={bar.id}
                onSelect={onSelectBar}
                selected={selectedSequence === bar.sequence}
              />
            ))}
      </div>
    </li>
  );
}

export { Swimlane };
