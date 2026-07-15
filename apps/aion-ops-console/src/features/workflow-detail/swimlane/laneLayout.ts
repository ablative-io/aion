import type {
  ActivityTimelineEntry,
  ChildWorkflowTimelineEntry,
  TimelineEntry,
  TimerTimelineEntry,
} from '../types';

/**
 * Pure lane-assignment + x-rank math for the swimlane (§4.1). No DOM, no React —
 * fully unit-testable. Consumes the S3 `projectTimeline` output (the TimelineEntry
 * union); it never re-parses raw events. Ordering is strictly by `seq` (the
 * determinism key, VISION invariant 2): the x axis is a DENSE RANK over the set of
 * sequences present, so bars line up on a discrete grid regardless of seq gaps.
 */

export type LaneKind = 'lifecycle' | 'activity' | 'timer' | 'signal' | 'child' | 'generic';

/** A bar's resolved status, used for color/shape (not gradients — VISION §1). */
export type BarStatus =
  | 'running'
  | 'completed'
  | 'failed'
  | 'cancelled'
  | 'timed-out'
  | 'fired'
  | 'marker';

export type SwimlaneBar = {
  /** Stable id (the entry id) for React keys + selection. */
  id: string;
  /** The seq used for selection + DetailPanel lookup. */
  sequence: number;
  /** Dense-rank x position of the bar's first event (0-based). */
  startRank: number;
  /** Dense-rank x position of the bar's last event (>= startRank). */
  endRank: number;
  /** True when the bar spans a single rank (a point marker, not a span). */
  isMarker: boolean;
  status: BarStatus;
  label: string;
  /** Drill-link target for child bars; null otherwise. */
  childWorkflowId: string | null;
  /**
   * One-based attempt segment boundaries (dense-ranks) for a retried activity, so
   * `LaneBar` can render segmented attempts (driven by `ActivityFailed.attempt`).
   * Empty for non-activity or single-attempt bars.
   */
  attemptRanks: number[];
  entry: TimelineEntry;
};

export type SwimlaneLane = {
  /** Stable lane id (kind + correlation key). */
  id: string;
  kind: LaneKind;
  label: string;
  /** Child workflow id for a child lane (the expansion target); null otherwise. */
  childWorkflowId: string | null;
  bars: SwimlaneBar[];
};

export type SwimlaneLayout = {
  lanes: SwimlaneLane[];
  /** Total number of distinct dense ranks (the x-axis width in grid columns). */
  rankCount: number;
  /** Ordered distinct sequences; index === dense rank. */
  sequences: number[];
};

/**
 * Build a dense-rank index over every sequence carried by the entries (each entry
 * may carry several correlated events, e.g. an activity's schedule/start/terminal).
 */
export function buildRankIndex(entries: readonly TimelineEntry[]): Map<number, number> {
  const seqs = new Set<number>();

  for (const entry of entries) {
    for (const event of entry.events) {
      seqs.add(event.data.envelope.seq);
    }
    seqs.add(entry.sequence);
  }

  const ordered = [...seqs].sort((left, right) => left - right);
  const index = new Map<number, number>();

  for (const [rank, seq] of ordered.entries()) {
    index.set(seq, rank);
  }

  return index;
}

/** Assemble the full lane layout from a projected timeline. */
export function layoutSwimlane(entries: readonly TimelineEntry[]): SwimlaneLayout {
  const rankIndex = buildRankIndex(entries);
  const sequences = [...rankIndex.entries()]
    .sort((left, right) => left[1] - right[1])
    .map(([seq]) => seq);

  const lifecycleBars: SwimlaneBar[] = [];
  const signalBars: SwimlaneBar[] = [];
  const genericBars: SwimlaneBar[] = [];
  const activityLanes = new Map<string, SwimlaneLane>();
  const timerLanes = new Map<string, SwimlaneLane>();
  const childLanes = new Map<string, SwimlaneLane>();

  for (const entry of [...entries].sort((left, right) => left.sequence - right.sequence)) {
    switch (entry.kind) {
      case 'lifecycle':
        lifecycleBars.push(markerBar(entry, rankIndex, lifecycleStatus(entry.outcome)));
        break;
      case 'signal':
        signalBars.push(markerBar(entry, rankIndex, 'marker'));
        break;
      case 'generic':
        genericBars.push(markerBar(entry, rankIndex, 'marker'));
        break;
      case 'activity':
        appendBar(activityLanes, activityLaneId(entry), entry.activityType ?? entry.activityId, {
          ...spanBar(entry, rankIndex, activityStatus(entry.status)),
          attemptRanks: attemptRanks(entry, rankIndex),
        });
        break;
      case 'timer':
        appendBar(
          timerLanes,
          `timer:${entry.timerId}`,
          `Timer ${entry.timerId}`,
          spanBar(entry, rankIndex, timerStatus(entry.status))
        );
        break;
      case 'child':
        appendBar(
          childLanes,
          `child:${entry.childWorkflowId}`,
          childLabel(entry),
          {
            ...spanBar(entry, rankIndex, childStatus(entry.status)),
            childWorkflowId: entry.childWorkflowId,
          },
          entry.childWorkflowId
        );
        break;
      default:
        break;
    }
  }

  const lanes: SwimlaneLane[] = [];

  if (lifecycleBars.length > 0) {
    lanes.push({
      id: 'lifecycle',
      kind: 'lifecycle',
      label: 'Lifecycle',
      childWorkflowId: null,
      bars: lifecycleBars,
    });
  }
  lanes.push(...sortLanes(activityLanes));
  lanes.push(...sortLanes(timerLanes));
  lanes.push(...sortLanes(childLanes));
  if (signalBars.length > 0) {
    lanes.push({
      id: 'signal',
      kind: 'signal',
      label: 'Signals',
      childWorkflowId: null,
      bars: signalBars,
    });
  }
  if (genericBars.length > 0) {
    lanes.push({
      id: 'generic',
      kind: 'generic',
      label: 'Other',
      childWorkflowId: null,
      bars: genericBars,
    });
  }

  return { lanes, rankCount: sequences.length, sequences };
}

function activityLaneId(entry: ActivityTimelineEntry): string {
  return `activity:${entry.activityId}`;
}

function childLabel(entry: ChildWorkflowTimelineEntry): string {
  return `Child ${entry.workflowType ?? entry.childWorkflowId}`;
}

function appendBar(
  lanes: Map<string, SwimlaneLane>,
  id: string,
  label: string,
  bar: SwimlaneBar,
  childWorkflowId: string | null = null
): void {
  const lane = lanes.get(id) ?? {
    id,
    kind: bar.entry.kind as LaneKind,
    label,
    childWorkflowId,
    bars: [],
  };

  if (!lanes.has(id)) {
    lanes.set(id, lane);
  }

  lane.bars.push(bar);
}

function sortLanes(lanes: Map<string, SwimlaneLane>): SwimlaneLane[] {
  return [...lanes.values()].sort((left, right) => firstRank(left) - firstRank(right));
}

function firstRank(lane: SwimlaneLane): number {
  return Math.min(...lane.bars.map((bar) => bar.startRank));
}

function rankOf(seq: number, rankIndex: Map<number, number>): number {
  return rankIndex.get(seq) ?? 0;
}

function eventRanks(entry: TimelineEntry, rankIndex: Map<number, number>): number[] {
  return entry.events.map((event) => rankOf(event.data.envelope.seq, rankIndex));
}

function spanBar(
  entry: TimelineEntry,
  rankIndex: Map<number, number>,
  status: BarStatus
): SwimlaneBar {
  const ranks = eventRanks(entry, rankIndex);
  const startRank = ranks.length > 0 ? Math.min(...ranks) : rankOf(entry.sequence, rankIndex);
  const endRank = ranks.length > 0 ? Math.max(...ranks) : startRank;

  return {
    id: entry.id,
    sequence: entry.sequence,
    startRank,
    endRank,
    isMarker: startRank === endRank,
    status,
    label: entry.summary,
    childWorkflowId: null,
    attemptRanks: [],
    entry,
  };
}

function markerBar(
  entry: TimelineEntry,
  rankIndex: Map<number, number>,
  status: BarStatus
): SwimlaneBar {
  const rank = rankOf(entry.sequence, rankIndex);

  return {
    id: entry.id,
    sequence: entry.sequence,
    startRank: rank,
    endRank: rank,
    isMarker: true,
    status,
    label: entry.summary,
    childWorkflowId: null,
    attemptRanks: [],
    entry,
  };
}

function attemptRanks(entry: ActivityTimelineEntry, rankIndex: Map<number, number>): number[] {
  if (entry.failures.length === 0) {
    return [];
  }

  return entry.failures
    .map((failure) => rankOf(failure.sequence, rankIndex))
    .sort((left, right) => left - right);
}

function lifecycleStatus(outcome: ActivityTimelineEntry['status'] | string): BarStatus {
  switch (outcome) {
    case 'completed':
      return 'completed';
    case 'failed':
      return 'failed';
    case 'cancelled':
      return 'cancelled';
    case 'timed-out':
      return 'timed-out';
    default:
      return 'marker';
  }
}

function activityStatus(status: ActivityTimelineEntry['status']): BarStatus {
  switch (status) {
    case 'completed':
      return 'completed';
    case 'failed':
      return 'failed';
    case 'cancelled':
      return 'cancelled';
    default:
      return 'running';
  }
}

function timerStatus(status: TimerTimelineEntry['status']): BarStatus {
  switch (status) {
    case 'fired':
      return 'fired';
    case 'completed':
      return 'completed';
    case 'cancelled':
      return 'cancelled';
    case 'timed-out':
      return 'timed-out';
    default:
      return 'running';
  }
}

function childStatus(status: ChildWorkflowTimelineEntry['status']): BarStatus {
  switch (status) {
    case 'completed':
      return 'completed';
    case 'failed':
      return 'failed';
    case 'cancelled':
      return 'cancelled';
    default:
      return 'running';
  }
}
