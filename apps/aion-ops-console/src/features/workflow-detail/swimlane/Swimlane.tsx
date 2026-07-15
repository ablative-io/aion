import { type ReactNode, useMemo, useState } from 'react';

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
  /**
   * Injected renderer for an expanded child lane's embedded run view. When
   * absent NO expansion affordance renders (pure layout call sites unchanged);
   * injection (not a direct import) keeps the module graph acyclic.
   */
  renderChildRun?: ((childWorkflowId: string) => ReactNode) | undefined;
  /** Child workflow ids whose run regions start expanded (tests only). */
  initialExpandedChildren?: readonly string[] | undefined;
};

/** A child lane narrowed to a non-null expansion target. */
type ExpandableLane = SwimlaneLane & { childWorkflowId: string };

/**
 * Partial-order swimlane (VISION §4.1). Renders the workflow as concurrent
 * horizontal lanes keyed on `seq` (not a linear list): a lifecycle lane, one lane
 * per activity / timer / child, plus signal + other lanes. Bars are positioned by
 * dense-seq-rank so concurrent work overlaps across lanes. Selecting a bar lifts its
 * seq + x-origin so the bottom-docked detail sheet morphs out of it. Lanes
 * collapse/expand. A child lane additionally offers a run-expand toggle that
 * mounts the injected embedded run view beneath the chart (and unmounts it on
 * collapse, closing its live subscription). This view
 * consumes the S3 `projectTimeline` output ONLY (no event re-parsing, no fabricated
 * data) — it grows in place as live events extend the projected entries.
 */
function Swimlane({
  entries,
  selectedSequence,
  onSelect,
  scrubSeq = null,
  renderChildRun,
  initialExpandedChildren,
}: SwimlaneProps) {
  const layout = useMemo(() => layoutSwimlane(prefixUpTo(entries, scrubSeq)), [entries, scrubSeq]);
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(() => new Set());
  const [expandedChildren, setExpandedChildren] = useState<ReadonlySet<string>>(
    () => new Set(initialExpandedChildren ?? [])
  );

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

  function toggleChildRun(childWorkflowId: string) {
    setExpandedChildren((current) => {
      const next = new Set(current);
      if (next.has(childWorkflowId)) {
        next.delete(childWorkflowId);
      } else {
        next.add(childWorkflowId);
      }
      return next;
    });
  }

  function handleSelect(bar: SwimlaneBar, originX: number) {
    onSelect(bar.sequence, { x: originX });
  }

  // The expanded child regions render BENEATH the horizontal-scroll section, not
  // inside the scrollport: a full run view pinned to the parent's dense-rank
  // width would be unreadable when the parent is scrolled right (and pathological
  // at depth) — each embedded run view brings its own overflow-x-auto swimlane.
  const expandedLanes: ExpandableLane[] =
    renderChildRun === undefined
      ? []
      : layout.lanes.filter(
          (lane): lane is ExpandableLane =>
            lane.childWorkflowId !== null && expandedChildren.has(lane.childWorkflowId)
        );

  return (
    <div className="space-y-2">
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
            {layout.lanes.map((lane) => {
              const childId = lane.childWorkflowId;
              return (
                <LaneRow
                  childWorkflowId={childId}
                  collapsed={collapsed.has(lane.id)}
                  key={lane.id}
                  lane={lane}
                  onSelectBar={handleSelect}
                  onToggle={() => toggleLane(lane.id)}
                  onToggleChildRun={
                    childId !== null && renderChildRun !== undefined
                      ? () => toggleChildRun(childId)
                      : null
                  }
                  runExpanded={childId !== null && expandedChildren.has(childId)}
                  selectedSequence={selectedSequence}
                  trackWidth={trackWidth}
                />
              );
            })}
          </ul>
        </div>
      </section>
      {expandedLanes.map((lane) => (
        <section
          aria-label={`Child run ${lane.childWorkflowId}`}
          className="rounded-xl border border-border border-l-4 border-l-primary/40 bg-surface-base p-3 pl-4"
          data-testid={`child-run:${lane.childWorkflowId}`}
          key={`child-run:${lane.id}`}
        >
          {renderChildRun?.(lane.childWorkflowId)}
        </section>
      ))}
    </div>
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
  childWorkflowId,
  runExpanded,
  onToggleChildRun,
}: {
  lane: SwimlaneLane;
  collapsed: boolean;
  trackWidth: number;
  selectedSequence: number | null;
  onToggle: () => void;
  onSelectBar: (bar: SwimlaneBar, originX: number) => void;
  childWorkflowId: string | null;
  runExpanded: boolean;
  /** Expand/collapse the child's embedded run view; null = no affordance. */
  onToggleChildRun: (() => void) | null;
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
      {childWorkflowId !== null && onToggleChildRun !== null ? (
        <button
          aria-expanded={runExpanded}
          aria-label={`${runExpanded ? 'Collapse' : 'Expand'} child run ${childWorkflowId}`}
          className="shrink-0 rounded px-1 text-muted-foreground text-xs hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          data-testid={`expand-child:${childWorkflowId}`}
          onClick={onToggleChildRun}
          type="button"
        >
          {runExpanded ? '▾' : '▸'}
        </button>
      ) : null}
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
