import { type CSSProperties, memo, type RefCallback, useCallback, useMemo } from 'react';
import { EventIcon, type EventIconKind } from '@/components/EventIcon';
import { Button } from '@/components/ui';
import { cn } from '@/lib/utils';
import type { WorkflowId } from '@/types';

import { LaneBar } from './LaneBar';
import type { SwimlaneBar, SwimlaneLane } from './laneLayout';
import type { LaneTreeRow } from './laneTree';
import {
  type AxisLayout,
  type AxisMode,
  clusterPositionedBars,
  type LaneScrubClip,
  MIN_BAR_WIDTH,
  positionBar,
} from './timeLayout';

const LANE_HEIGHT = 36;
const DEPTH_INDENT = 16;

export function ChartToolbar({
  axisMode,
  onChange,
}: {
  axisMode: AxisMode;
  onChange: (mode: AxisMode) => void;
}) {
  return (
    <div className="flex items-center justify-between border-border border-b bg-surface-base px-3 py-2">
      <span className="text-muted-foreground text-xs uppercase tracking-wide">Axis</span>
      <fieldset
        aria-label="Swimlane axis"
        className="inline-flex rounded-lg border border-border p-0.5"
      >
        <Button
          aria-pressed={axisMode === 'time'}
          className="h-7 px-3 text-xs"
          onClick={() => onChange('time')}
          type="button"
          variant="ghost"
        >
          Time
        </Button>
        <Button
          aria-pressed={axisMode === 'stepped'}
          className="h-7 px-3 text-xs"
          onClick={() => onChange('stepped')}
          type="button"
          variant="ghost"
        >
          Stepped
        </Button>
      </fieldset>
    </div>
  );
}

export function Axis({ axis, labelWidth }: { axis: AxisLayout | null; labelWidth: number }) {
  return (
    <div className="flex h-7 border-border border-b bg-surface-base">
      <div
        className="shrink-0 px-3 py-1 text-muted-foreground text-xs uppercase tracking-wide"
        style={{ width: labelWidth }}
      >
        Lane
      </div>
      <div className="relative min-w-0 flex-1" style={{ width: axis?.trackWidth ?? 0 }}>
        {axis === null ? null : axis.mode === 'time' ? (
          <>
            <span className="absolute top-1 left-1 text-[10px] text-muted-foreground tabular-nums">
              {formatAxisTime(axis.bounds.t0)}
            </span>
            <span className="absolute top-1 right-1 text-[10px] text-muted-foreground tabular-nums">
              {formatAxisTime(axis.bounds.tEnd)}
            </span>
          </>
        ) : (
          axis.events.map((event, rank) => (
            <span
              className="absolute top-1 text-[10px] text-muted-foreground tabular-nums"
              key={event.key}
              style={{ left: rank * axis.rankWidth + 4 }}
              title={`${event.workflowId} · ${event.recordedAt}`}
            >
              {event.sequence}
            </span>
          ))
        )}
      </div>
    </div>
  );
}

type VirtualRow = {
  index: number;
  start: number;
  measureElement: RefCallback<HTMLLIElement>;
};

type LaneRowProps = {
  lane: SwimlaneLane;
  depth: number;
  collapsed: boolean;
  axis: AxisLayout;
  trackWidth: number;
  workflowId: string;
  selectedSequence: number | null;
  selectedWorkflowId: WorkflowId | null;
  childExpanded: boolean;
  /** Active scrub cursor + the lane's untruncated entries; null when live. */
  scrubClip?: LaneScrubClip | null;
  onToggle: () => void;
  onToggleChild: (() => void) | null;
  onSelect: (bar: SwimlaneBar, originX: number, clusterSequences: readonly number[]) => void;
  virtual?: VirtualRow;
};

export const LaneRow = memo(function LaneRow({
  lane,
  depth,
  collapsed,
  axis,
  trackWidth,
  workflowId,
  selectedSequence,
  selectedWorkflowId,
  childExpanded,
  scrubClip = null,
  onToggle,
  onToggleChild,
  onSelect,
  virtual,
}: LaneRowProps) {
  const items = useMemo(
    () =>
      clusterPositionedBars(
        lane.bars.map((bar) =>
          positionBar(
            workflowId,
            bar,
            axis,
            MIN_BAR_WIDTH,
            scrubClip === null
              ? null
              : {
                  cursor: scrubClip.cursor,
                  fullEntry: scrubClip.fullEntriesById.get(bar.id) ?? bar.entry,
                }
          )
        )
      ),
    [axis, lane.bars, scrubClip, workflowId]
  );
  const selectSingleBar = useCallback(
    (bar: SwimlaneBar, originX: number) => onSelect(bar, originX, [bar.sequence]),
    [onSelect]
  );
  const virtualStyle: CSSProperties | undefined =
    virtual === undefined
      ? undefined
      : {
          position: 'absolute',
          top: 0,
          left: 0,
          width: '100%',
          transform: `translateY(${virtual.start}px)`,
        };

  return (
    <li
      className="flex items-stretch"
      data-depth={depth}
      data-index={virtual?.index}
      data-workflow-id={workflowId}
      ref={virtual?.measureElement}
      style={virtualStyle}
    >
      <div className="flex w-[168px] shrink-0 items-stretch">
        <button
          aria-expanded={!collapsed}
          className="flex min-w-0 flex-1 items-center gap-2 py-2 pr-1 text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          onClick={onToggle}
          style={{ paddingLeft: 12 + depth * DEPTH_INDENT }}
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
        {onToggleChild === null ? null : (
          <button
            aria-expanded={childExpanded}
            aria-label={`${childExpanded ? 'Collapse' : 'Expand'} child lanes ${lane.childWorkflowId}`}
            className="shrink-0 rounded px-1 text-muted-foreground text-xs hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            data-testid={`expand-child:${lane.childWorkflowId}`}
            onClick={onToggleChild}
            type="button"
          >
            {childExpanded ? '▾' : '▸'}
          </button>
        )}
      </div>
      <div
        className={cn('relative', collapsed && 'opacity-40')}
        style={{ width: trackWidth, height: collapsed ? LANE_HEIGHT / 2 : LANE_HEIGHT }}
      >
        {collapsed
          ? null
          : items.map((item) => {
              if (item.kind === 'cluster') {
                const first = item.members[0];
                if (first === undefined) {
                  return null;
                }
                const sequences = item.members.map((member) => member.bar.sequence);
                return (
                  <button
                    aria-label={`${item.members.length} events; open first event`}
                    className="absolute top-1 z-20 flex h-7 min-w-7 items-center justify-center rounded-md border border-primary/50 bg-primary/20 px-1 text-[10px] text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                    data-cluster-count={item.members.length}
                    key={`cluster:${workflowId}:${sequences.join(':')}`}
                    onClick={(event) => {
                      const rect = event.currentTarget.getBoundingClientRect();
                      onSelect(first.bar, rect.left + rect.width / 2, sequences);
                    }}
                    style={{ left: item.left, width: Math.max(28, item.width) }}
                    type="button"
                  >
                    ×{item.members.length}
                  </button>
                );
              }
              return (
                <LaneBar
                  key={item.positioned.bar.id}
                  onSelect={selectSingleBar}
                  positioned={item.positioned}
                  selected={
                    selectedWorkflowId === workflowId &&
                    selectedSequence === item.positioned.bar.sequence
                  }
                />
              );
            })}
      </div>
    </li>
  );
});

export function NoticeRow({
  row,
  labelWidth,
  virtual,
}: {
  row: Extract<LaneTreeRow, { kind: 'notice' }>;
  labelWidth: number;
  virtual?: VirtualRow;
}) {
  const virtualStyle: CSSProperties | undefined =
    virtual === undefined
      ? undefined
      : {
          position: 'absolute',
          top: 0,
          left: 0,
          width: '100%',
          transform: `translateY(${virtual.start}px)`,
        };
  return (
    <li
      className="flex h-9 items-center"
      data-depth={row.depth}
      data-index={virtual?.index}
      data-notice={row.notice}
      ref={virtual?.measureElement}
      style={virtualStyle}
    >
      <div
        className="shrink-0 truncate text-muted-foreground text-xs"
        style={{ width: labelWidth, paddingLeft: 12 + row.depth * DEPTH_INDENT }}
      >
        {row.workflowId}
      </div>
      <div className="min-w-0 flex-1 truncate px-2 text-muted-foreground text-xs">
        {row.message}
      </div>
    </li>
  );
}

function formatAxisTime(timestampMs: number): string {
  return new Date(timestampMs).toISOString().replace('.000Z', 'Z');
}
