import { useCallback, useEffect, useMemo, useRef, useState } from 'react';

import { EmptyState } from '@/components/EmptyState';
import { cn } from '@/lib/utils';
import type { WorkflowId } from '@/types';

import type { TimelineEntry } from '../types';
import { ChildTimelineLoader } from './ChildTimelineLoader';
import { layoutSwimlane, type SwimlaneBar, type SwimlaneLane } from './laneLayout';
import {
  type ChildTimelineState,
  childNodePath,
  flattenLaneTree,
  timerGroupPath,
} from './laneTree';
import { Scrubber } from './Scrubber';
import { Axis, ChartToolbar, LaneRow, NoticeRow } from './SwimlaneRows';
import { cutsAtGlobalRank, cutsAtTimestamp, prefixUpTo, snapGlobalRank } from './scrub';
import { type AxisMode, buildAxisLayout, type LaneScrubClip } from './timeLayout';

export const LANE_LABEL_WIDTH = 168;
const NO_EXPANDED_CHILDREN: readonly string[] = [];

export type SwimlaneSelection = {
  workflowId: WorkflowId;
  sequence: number;
  timeline: readonly TimelineEntry[];
  clusterSequences: readonly number[];
};

/** The exact payload emitted by a parent or depth-N bar click. */
export function selectionForBar(
  workflowId: WorkflowId,
  timeline: readonly TimelineEntry[],
  bar: SwimlaneBar,
  clusterSequences: readonly number[]
): SwimlaneSelection {
  return { workflowId, sequence: bar.sequence, timeline, clusterSequences };
}

export type SwimlaneProps = {
  workflowId: WorkflowId;
  entries: readonly TimelineEntry[];
  isRunning: boolean;
  selectedSequence: number | null;
  selectedWorkflowId?: WorkflowId | null;
  onSelect: (selection: SwimlaneSelection, origin?: { x: number }) => void;
  scrubPosition?: number | null;
  onScrubChange?: (position: number | null) => void;
  initialAxisMode?: AxisMode;
  initialExpandedChildren?: readonly string[];
  /** Seeded child path states keep recursive SSR/pure rendering deterministic in tests. */
  initialChildTimelines?: ReadonlyMap<string, ChildTimelineState>;
  /** Production enables hook-backed lazy children; pure SSR tests may explicitly disable it. */
  loadChildren?: boolean;
  nowMs?: number;
};

type LaneRowHandlers = {
  onSelect: (bar: SwimlaneBar, originX: number, clusterSequences: readonly number[]) => void;
  onToggle: () => void;
  onToggleChild: (() => void) | null;
  onToggleTimerGroup: (() => void) | null;
};

/**
 * One shared chart for a workflow tree: time fits the viewport, stepped mode uses
 * the visible global event order, and expanded child workflows splice true rows
 * directly beneath their parent child lane.
 */
function Swimlane({
  workflowId,
  entries,
  isRunning,
  selectedSequence,
  selectedWorkflowId = workflowId,
  onSelect,
  scrubPosition = null,
  onScrubChange,
  initialAxisMode = 'time',
  initialExpandedChildren = NO_EXPANDED_CHILDREN,
  initialChildTimelines,
  loadChildren = false,
  nowMs: nowOverride,
}: SwimlaneProps) {
  const [axisMode, setAxisMode] = useState<AxisMode>(initialAxisMode);
  const [expandedPaths, setExpandedPaths] = useState<ReadonlySet<string>>(() => new Set());
  const [collapsedPaths, setCollapsedPaths] = useState<ReadonlySet<string>>(() => new Set());
  const [collapsedRows, setCollapsedRows] = useState<ReadonlySet<string>>(() => new Set());
  const [childTimelines, setChildTimelines] = useState<ReadonlyMap<string, ChildTimelineState>>(
    () => new Map(initialChildTimelines)
  );
  const initialExpandedIds = useMemo(
    () => new Set(initialExpandedChildren),
    [initialExpandedChildren]
  );
  const tree = useMemo(
    () =>
      flattenLaneTree(
        { workflowId, entries, isRunning },
        childTimelines,
        expandedPaths,
        initialExpandedIds,
        collapsedPaths
      ),
    [
      childTimelines,
      collapsedPaths,
      entries,
      expandedPaths,
      initialExpandedIds,
      isRunning,
      workflowId,
    ]
  );
  const containerRef = useRef<HTMLElement | null>(null);
  const containerWidth = useObservedWidth(containerRef);
  const nowMs = useCurrentTime(
    axisMode === 'time' && tree.workflows.some((item) => item.isRunning),
    nowOverride
  );
  const axis = useMemo(
    () =>
      buildAxisLayout(
        tree.workflows,
        axisMode,
        Math.max(0, containerWidth - LANE_LABEL_WIDTH),
        nowMs
      ),
    [axisMode, containerWidth, nowMs, tree.workflows]
  );
  const cuts = useMemo(() => {
    if (scrubPosition === null) {
      return null;
    }
    return axisMode === 'time'
      ? cutsAtTimestamp(tree.workflows, scrubPosition)
      : cutsAtGlobalRank(tree.workflows, scrubPosition);
  }, [axisMode, scrubPosition, tree.workflows]);
  const hasRootLanes = useMemo(
    () => tree.rows.some((row) => row.kind === 'lane' && row.path === workflowId),
    [tree.rows, workflowId]
  );
  const timelinesByPath = useMemo(() => {
    const timelines = new Map<string, readonly TimelineEntry[]>();
    for (const row of tree.rows) {
      if (!timelines.has(row.path)) {
        timelines.set(row.path, timelineForPath(row.path, workflowId, entries, childTimelines));
      }
    }
    return timelines;
  }, [childTimelines, entries, tree.rows, workflowId]);
  const fullEntriesByPath = useMemo(() => {
    const entriesByPath = new Map<string, ReadonlyMap<string, TimelineEntry>>();
    if (cuts === null) {
      return entriesByPath;
    }
    for (const [path, timeline] of timelinesByPath) {
      entriesByPath.set(path, new Map(timeline.map((entry) => [entry.id, entry])));
    }
    return entriesByPath;
  }, [cuts, timelinesByPath]);
  const scrubLanesByPath = useMemo(() => {
    const lanesByPath = new Map<string, ReadonlyMap<string, SwimlaneLane>>();
    if (cuts === null) {
      return lanesByPath;
    }
    const workflowIdByPath = new Map(tree.rows.map((row) => [row.path, row.workflowId]));
    for (const [path, timeline] of timelinesByPath) {
      const pathWorkflowId = workflowIdByPath.get(path);
      const cut = pathWorkflowId === undefined ? undefined : cuts.get(pathWorkflowId);
      const visibleEntries = cut === null || cut === undefined ? [] : prefixUpTo(timeline, cut);
      const lanes = layoutSwimlane(visibleEntries, {
        expandTimers: expandedPaths.has(timerGroupPath(path)),
      }).lanes;
      lanesByPath.set(path, new Map(lanes.map((lane) => [lane.id, lane])));
    }
    return lanesByPath;
  }, [cuts, expandedPaths, timelinesByPath, tree.rows]);
  /** The clip cursor in the active axis coordinate: ms in time, rank in stepped. */
  const scrubCursor =
    scrubPosition === null || axis === null
      ? null
      : axisMode === 'time'
        ? scrubPosition
        : snapGlobalRank(scrubPosition, axis.events.length);
  const scrubClipsByPath = useMemo(() => {
    const clips = new Map<string, LaneScrubClip>();
    if (scrubCursor === null) {
      return clips;
    }
    for (const [path, fullEntriesById] of fullEntriesByPath) {
      clips.set(path, { cursor: scrubCursor, fullEntriesById });
    }
    return clips;
  }, [fullEntriesByPath, scrubCursor]);

  const updateChildState = useCallback((path: string, state: ChildTimelineState) => {
    setChildTimelines((current) => {
      if (current.get(path) === state) {
        return current;
      }
      const next = new Map(current);
      next.set(path, state);
      return next;
    });
  }, []);

  const setMode = useCallback(
    (mode: AxisMode) => {
      setAxisMode(mode);
      onScrubChange?.(null);
    },
    [onScrubChange]
  );
  const toggleRow = useCallback((id: string) => {
    setCollapsedRows((current) => toggleSetMember(current, id));
  }, []);
  const toggleChild = useCallback(
    (parentPath: string, childWorkflowId: string) => {
      const path = childNodePath(parentPath, childWorkflowId);
      const expanded =
        !collapsedPaths.has(path) &&
        (expandedPaths.has(path) || initialExpandedIds.has(childWorkflowId));
      if (expanded) {
        setCollapsedPaths((current) => new Set(current).add(path));
        setExpandedPaths((current) => withoutPathAndDescendants(current, path));
      } else {
        setCollapsedPaths((current) => {
          const next = new Set(current);
          next.delete(path);
          return next;
        });
        setExpandedPaths((current) => new Set(current).add(path));
      }
    },
    [collapsedPaths, expandedPaths, initialExpandedIds]
  );
  const toggleTimerGroup = useCallback((path: string) => {
    setExpandedPaths((current) => toggleSetMember(current, timerGroupPath(path)));
  }, []);
  const rowHandlers = useMemo(() => {
    const handlers = new Map<string, LaneRowHandlers>();
    for (const row of tree.rows) {
      if (row.kind !== 'lane') {
        continue;
      }
      const timeline = timelinesByPath.get(row.path) ?? [];
      const childId = row.lane.childWorkflowId;
      handlers.set(row.id, {
        onSelect: (bar, originX, clusterSequences) =>
          onSelect(selectionForBar(row.workflowId as WorkflowId, timeline, bar, clusterSequences), {
            x: originX,
          }),
        onToggle: () => toggleRow(row.id),
        onToggleChild: childId === null ? null : () => toggleChild(row.path, childId),
        onToggleTimerGroup:
          row.timerGroupExpanded === null ? null : () => toggleTimerGroup(row.path),
      });
    }
    return handlers;
  }, [onSelect, timelinesByPath, toggleChild, toggleRow, toggleTimerGroup, tree.rows]);
  const handleScrub = useCallback(
    (position: number | null) => onScrubChange?.(position),
    [onScrubChange]
  );

  if (!hasRootLanes) {
    return (
      <EmptyState description="No correlated events to lay out yet." title="Nothing to visualize" />
    );
  }

  const trackWidth = axis?.trackWidth ?? 0;
  const innerWidth =
    axisMode === 'stepped' ? LANE_LABEL_WIDTH + trackWidth : Math.max(0, containerWidth);

  return (
    <>
      <section
        aria-label="Workflow swimlane"
        className={cn(
          'rounded-xl border border-border bg-surface-elevated',
          axisMode === 'stepped' ? 'overflow-x-auto' : 'overflow-x-hidden'
        )}
        data-axis-mode={axisMode}
        ref={containerRef}
      >
        <ChartToolbar axisMode={axisMode} onChange={setMode} />
        <div style={{ minWidth: innerWidth }}>
          <Axis axis={axis} labelWidth={LANE_LABEL_WIDTH} />
          <ul aria-label="Lanes" className="divide-y divide-border">
            {tree.rows.map((row) => {
              if (row.kind === 'notice') {
                return <NoticeRow key={row.id} labelWidth={LANE_LABEL_WIDTH} row={row} />;
              }
              const lane =
                cuts === null ? row.lane : scrubLanesByPath.get(row.path)?.get(row.lane.id);
              const handlers = rowHandlers.get(row.id);
              if (lane === undefined || axis === null || handlers === undefined) {
                return null;
              }
              const childId = row.lane.childWorkflowId;
              const childPath = childId === null ? null : childNodePath(row.path, childId);
              const childExpanded =
                childId !== null &&
                childPath !== null &&
                !collapsedPaths.has(childPath) &&
                (expandedPaths.has(childPath) || initialExpandedIds.has(childId));
              return (
                <LaneRow
                  axis={axis}
                  childExpanded={childExpanded}
                  collapsed={collapsedRows.has(row.id)}
                  depth={row.depth}
                  key={row.id}
                  lane={lane}
                  onSelect={handlers.onSelect}
                  onToggle={handlers.onToggle}
                  onToggleChild={handlers.onToggleChild}
                  onToggleTimerGroup={handlers.onToggleTimerGroup}
                  scrubClip={scrubClipsByPath.get(row.path) ?? null}
                  selectedSequence={selectedSequence}
                  selectedWorkflowId={selectedWorkflowId}
                  timerGroupExpanded={row.timerGroupExpanded}
                  trackWidth={trackWidth}
                  workflowId={row.workflowId}
                />
              );
            })}
          </ul>
        </div>
        <Scrubber axis={axis} mode={axisMode} onScrub={handleScrub} scrubPosition={scrubPosition} />
      </section>
      {loadChildren
        ? tree.requests.map((request) => (
            <ChildTimelineLoader
              key={request.path}
              onState={updateChildState}
              path={request.path}
              workflowId={request.workflowId as WorkflowId}
            />
          ))
        : null}
    </>
  );
}

function timelineForPath(
  path: string,
  rootWorkflowId: string,
  rootEntries: readonly TimelineEntry[],
  children: ReadonlyMap<string, ChildTimelineState>
): readonly TimelineEntry[] {
  if (path === rootWorkflowId) {
    return rootEntries;
  }
  return children.get(path)?.entries ?? [];
}

function toggleSetMember(current: ReadonlySet<string>, id: string): ReadonlySet<string> {
  const next = new Set(current);
  if (next.has(id)) {
    next.delete(id);
  } else {
    next.add(id);
  }
  return next;
}

function withoutPathAndDescendants(
  current: ReadonlySet<string>,
  path: string
): ReadonlySet<string> {
  return new Set(
    [...current].filter((candidate) => candidate !== path && !candidate.startsWith(`${path}>`))
  );
}

function useObservedWidth(ref: { current: HTMLElement | null }): number {
  const [width, setWidth] = useState(0);

  useEffect(() => {
    const element = ref.current;
    if (element === null || typeof ResizeObserver === 'undefined') {
      return;
    }
    const observer = new ResizeObserver((entries) => {
      const observed = entries[0];
      if (observed !== undefined) {
        setWidth(observed.contentRect.width);
      }
    });
    observer.observe(element);
    setWidth(element.getBoundingClientRect().width);
    return () => observer.disconnect();
  }, [ref]);

  return width;
}

function useCurrentTime(running: boolean, override: number | undefined): number {
  const [nowMs, setNowMs] = useState(() => override ?? Date.now());

  useEffect(() => {
    if (override !== undefined) {
      setNowMs(override);
      return;
    }
    if (!running) {
      return;
    }
    const interval = window.setInterval(() => setNowMs(Date.now()), 1000);
    return () => window.clearInterval(interval);
  }, [override, running]);

  return override ?? nowMs;
}

export { Swimlane };
