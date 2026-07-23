import type { TimelineEntry } from '../types';
import type { SwimlaneBar } from './laneLayout';

export type AxisMode = 'time' | 'stepped';

export type VisibleWorkflow = {
  workflowId: string;
  entries: readonly TimelineEntry[];
  isRunning: boolean;
};

export type OrderedTimelineEvent = {
  key: string;
  workflowId: string;
  sequence: number;
  recordedAt: string;
  recordedAtMs: number;
};

export type TimeBounds = {
  t0: number;
  tEnd: number;
};

export type AxisLayout = {
  mode: AxisMode;
  trackWidth: number;
  bounds: TimeBounds;
  events: readonly OrderedTimelineEvent[];
  rankByEvent: ReadonlyMap<string, number>;
  rankWidth: number;
  runningWorkflowIds: ReadonlySet<string>;
};

export type PositionedBar = {
  bar: SwimlaneBar;
  left: number;
  width: number;
  marker: boolean;
  attemptOffsets: number[];
};

export type PositionedBarItem =
  | { kind: 'bar'; positioned: PositionedBar }
  | {
      kind: 'cluster';
      left: number;
      width: number;
      members: readonly PositionedBar[];
    };

export const MIN_BAR_WIDTH = 6;
export const MIN_STEPPED_RANK_WIDTH = 24;

/**
 * Scrub-cursor clip for one bar. The scrubbed timeline truncates each entry's
 * events at the per-workflow cut, so a bar whose span STRADDLES the cursor
 * would otherwise snap back to its last surviving event (or collapse to a
 * marker) and only pop to full size once the cursor passed its end. Supplying
 * the bar's UNTRUNCATED entry lets `positionBar` draw the true span clipped at
 * the cursor: straddling bars flow with the cursor, spans entirely before it
 * stay fully drawn, and spans entirely after it never reach layout (their
 * entries are dropped by the prefix cut).
 */
export type ScrubClip = {
  /** Cursor position: recorded-time ms in time mode, global event rank in stepped mode. */
  cursor: number;
  /** The bar's untruncated projected entry — carries the true extent past the cut. */
  fullEntry: TimelineEntry;
};

/**
 * Per-lane-row clip payload: the shared cursor plus each bar's untruncated
 * entry, keyed by entry id (stable across the prefix cut).
 */
export type LaneScrubClip = {
  cursor: number;
  fullEntriesById: ReadonlyMap<string, TimelineEntry>;
};

export function eventKey(workflowId: string, sequence: number): string {
  return `${workflowId}:${sequence}`;
}

export function recordedAtMs(recordedAt: string): number {
  const parsed = Date.parse(recordedAt);
  if (Number.isNaN(parsed)) {
    throw new Error(`Invalid timeline recorded_at value: ${recordedAt}`);
  }
  return parsed;
}

/** All visible raw events, globally ordered by the operator-approved tuple. */
export function orderedVisibleEvents(
  workflows: readonly VisibleWorkflow[]
): OrderedTimelineEvent[] {
  const events = new Map<string, OrderedTimelineEvent>();

  for (const workflow of workflows) {
    for (const entry of workflow.entries) {
      for (const event of entry.events) {
        const sequence = event.data.envelope.seq;
        const key = eventKey(workflow.workflowId, sequence);
        if (!events.has(key)) {
          const recordedAt = event.data.envelope.recorded_at;
          events.set(key, {
            key,
            workflowId: workflow.workflowId,
            sequence,
            recordedAt,
            recordedAtMs: recordedAtMs(recordedAt),
          });
        }
      }
    }
  }

  return [...events.values()].sort(
    (left, right) =>
      left.recordedAtMs - right.recordedAtMs ||
      left.sequence - right.sequence ||
      left.workflowId.localeCompare(right.workflowId)
  );
}

/** Shared time extent across the parent and every expanded child workflow. */
export function timeBounds(
  workflows: readonly VisibleWorkflow[],
  nowMs: number
): TimeBounds | null {
  const events = orderedVisibleEvents(workflows);
  const first = events[0];
  const last = events.at(-1);
  if (first === undefined || last === undefined) {
    return null;
  }

  return {
    t0: first.recordedAtMs,
    tEnd: workflows.some((workflow) => workflow.isRunning)
      ? Math.max(last.recordedAtMs, nowMs)
      : last.recordedAtMs,
  };
}

export function fractionForTimestamp(timestampMs: number, bounds: TimeBounds): number {
  const duration = bounds.tEnd - bounds.t0;
  if (duration <= 0) {
    return 0;
  }
  return Math.min(1, Math.max(0, (timestampMs - bounds.t0) / duration));
}

/** Build either a fit-to-view linear axis or the fit/min-width stepped union axis. */
export function buildAxisLayout(
  workflows: readonly VisibleWorkflow[],
  mode: AxisMode,
  availableTrackWidth: number,
  nowMs: number
): AxisLayout | null {
  const events = orderedVisibleEvents(workflows);
  const bounds = timeBounds(workflows, nowMs);
  if (events.length === 0 || bounds === null) {
    return null;
  }

  const rankByEvent = new Map<string, number>();
  for (const [rank, event] of events.entries()) {
    rankByEvent.set(event.key, rank);
  }

  const fitWidth = Math.max(0, availableTrackWidth);
  const trackWidth =
    mode === 'time' ? fitWidth : Math.max(fitWidth, events.length * MIN_STEPPED_RANK_WIDTH);

  return {
    mode,
    trackWidth,
    bounds,
    events,
    rankByEvent,
    rankWidth: events.length === 0 ? 0 : trackWidth / events.length,
    runningWorkflowIds: new Set(
      workflows.filter((workflow) => workflow.isRunning).map((workflow) => workflow.workflowId)
    ),
  };
}

/** Resolve a projected bar onto the shared axis, including the accepted marker floor. */
export function positionBar(
  workflowId: string,
  bar: SwimlaneBar,
  axis: AxisLayout,
  minimumWidth = MIN_BAR_WIDTH,
  clip: ScrubClip | null = null
): PositionedBar {
  const spanEntry = clip === null ? bar.entry : clip.fullEntry;
  const points = spanEntry.events.map((event) => ({
    sequence: event.data.envelope.seq,
    timestampMs: recordedAtMs(event.data.envelope.recorded_at),
  }));
  const firstPoint = points[0] ?? {
    sequence: bar.sequence,
    timestampMs: axis.bounds.t0,
  };

  let rawLeft: number;
  let rawWidth: number;
  let pointSpan = bar.isMarker;

  if (axis.mode === 'time') {
    const timestamps = points.map((point) => point.timestampMs);
    const start = timestamps.length > 0 ? Math.min(...timestamps) : firstPoint.timestampMs;
    let end = timestamps.length > 0 ? Math.max(...timestamps) : start;
    if (clip === null && bar.status === 'running' && axis.runningWorkflowIds.has(workflowId)) {
      end = Math.max(end, axis.bounds.tEnd);
      pointSpan = end === start;
    } else if (clip !== null) {
      end = Math.max(start, Math.min(end, clip.cursor));
      pointSpan = end === start;
    }
    rawLeft = fractionForTimestamp(start, axis.bounds) * axis.trackWidth;
    rawWidth =
      (fractionForTimestamp(end, axis.bounds) - fractionForTimestamp(start, axis.bounds)) *
      axis.trackWidth;
  } else {
    const ranks = points.map(
      (point) => axis.rankByEvent.get(eventKey(workflowId, point.sequence)) ?? 0
    );
    const startRank = ranks.length > 0 ? Math.min(...ranks) : 0;
    let endRank = ranks.length > 0 ? Math.max(...ranks) : startRank;
    if (clip !== null) {
      endRank = Math.max(startRank, Math.min(endRank, clip.cursor));
      pointSpan = endRank === startRank;
    }
    rawLeft = startRank * axis.rankWidth;
    rawWidth = (endRank - startRank + 1) * axis.rankWidth - 4;
  }

  const marker = pointSpan || rawWidth < minimumWidth;
  const width = marker
    ? Math.min(
        Math.max(0, axis.trackWidth),
        Math.max(minimumWidth, Math.min(14, axis.rankWidth || 14))
      )
    : Math.max(minimumWidth, rawWidth);
  const left = marker
    ? Math.min(
        Math.max(0, axis.trackWidth - width),
        Math.max(0, rawLeft + rawWidth / 2 - width / 2)
      )
    : Math.min(Math.max(0, axis.trackWidth - width), Math.max(0, rawLeft));

  const attemptOffsets = bar.attemptSequences.map((sequence) => {
    const event = bar.entry.events.find((candidate) => candidate.data.envelope.seq === sequence);
    if (event === undefined) {
      return 0;
    }
    if (axis.mode === 'time') {
      return (
        fractionForTimestamp(recordedAtMs(event.data.envelope.recorded_at), axis.bounds) *
          axis.trackWidth -
        left
      );
    }
    return (axis.rankByEvent.get(eventKey(workflowId, sequence)) ?? 0) * axis.rankWidth - left;
  });

  return { bar, left, width, marker, attemptOffsets };
}

/** Collapse overlapping marker-floor neighbors into one burst chip. */
export function clusterPositionedBars(bars: readonly PositionedBar[]): PositionedBarItem[] {
  const ordered = [...bars].sort(
    (left, right) => left.left - right.left || left.bar.sequence - right.bar.sequence
  );
  const items: PositionedBarItem[] = [];
  let markerGroup: PositionedBar[] = [];

  function flushMarkers() {
    if (markerGroup.length === 1) {
      const positioned = markerGroup[0];
      if (positioned !== undefined) {
        items.push({ kind: 'bar', positioned });
      }
    } else if (markerGroup.length > 1) {
      const left = Math.min(...markerGroup.map((positioned) => positioned.left));
      const right = Math.max(
        ...markerGroup.map((positioned) => positioned.left + positioned.width)
      );
      items.push({ kind: 'cluster', left, width: right - left, members: markerGroup });
    }
    markerGroup = [];
  }

  for (const positioned of ordered) {
    if (!positioned.marker) {
      flushMarkers();
      items.push({ kind: 'bar', positioned });
      continue;
    }

    const previous = markerGroup.at(-1);
    if (previous !== undefined && positioned.left <= previous.left + previous.width) {
      markerGroup.push(positioned);
    } else {
      flushMarkers();
      markerGroup.push(positioned);
    }
  }
  flushMarkers();

  return items.sort((left, right) => itemLeft(left) - itemLeft(right));
}

function itemLeft(item: PositionedBarItem): number {
  return item.kind === 'bar' ? item.positioned.left : item.left;
}
