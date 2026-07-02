import { useEffect, useRef, useState } from 'react';

import { Button } from '@/components/ui';

import type { TranscriptEntry } from '../hooks/useTranscript';
import { TranscriptEntryRow } from './TranscriptEventRow';

/**
 * Bounded, virtualized transcript list (NOI-7).
 *
 * The list owns its scroll region and windows the rendered rows by hand (no
 * virtualization dependency exists in this app): unrendered rows are replaced by
 * spacer padding sized from measured heights (estimated until first paint, then
 * refined). New entries flow in at the bottom with a subtle entrance and the list
 * follows the tail — unless the operator has scrolled up, in which case it stays
 * put and offers a "Jump to latest" affordance instead of yanking them down.
 */

/** Estimated height (px) for a not-yet-measured row, refined after first paint. */
const ESTIMATED_ROW_HEIGHT_PX = 72;
/** Extra rows rendered above/below the viewport so scrolling never shows a gap. */
const OVERSCAN_ROWS = 8;
/** How close (px) to the bottom still counts as "following the tail". */
const FOLLOW_THRESHOLD_PX = 48;
/** Rows rendered from the tail before the viewport has been measured. */
const UNMEASURED_TAIL_ROWS = 200;

export type VirtualWindow = {
  /** First rendered row index (inclusive). */
  start: number;
  /** Last rendered row index (exclusive). */
  end: number;
  /** Spacer height (px) standing in for the rows above the window. */
  topPad: number;
  /** Spacer height (px) standing in for the rows below the window. */
  bottomPad: number;
};

/**
 * Compute the rendered row window for a scroll position. Before the viewport has
 * been measured (`viewportHeight <= 0`, including static/server renders) the tail
 * {@link UNMEASURED_TAIL_ROWS} rows render so the list is immediately meaningful.
 */
export function computeVirtualWindow(
  rowHeights: number[],
  scrollTop: number,
  viewportHeight: number
): VirtualWindow {
  const count = rowHeights.length;
  const offsets = prefixOffsets(rowHeights);
  if (viewportHeight <= 0) {
    const start = Math.max(0, count - UNMEASURED_TAIL_ROWS);
    return { start, end: count, topPad: offsets[start] ?? 0, bottomPad: 0 };
  }

  const total = offsets[count] ?? 0;
  let start = 0;
  while (start < count && (offsets[start + 1] ?? 0) <= scrollTop) {
    start += 1;
  }
  let end = start;
  while (end < count && (offsets[end] ?? 0) < scrollTop + viewportHeight) {
    end += 1;
  }
  start = Math.max(0, start - OVERSCAN_ROWS);
  end = Math.min(count, end + OVERSCAN_ROWS);

  return { start, end, topPad: offsets[start] ?? 0, bottomPad: total - (offsets[end] ?? 0) };
}

/** Prefix offsets: `offsets[i]` = top of row i; `offsets[count]` = total height. */
function prefixOffsets(rowHeights: number[]): number[] {
  const offsets: number[] = new Array(rowHeights.length + 1);
  offsets[0] = 0;
  for (let index = 0; index < rowHeights.length; index += 1) {
    offsets[index + 1] = (offsets[index] ?? 0) + (rowHeights[index] ?? ESTIMATED_ROW_HEIGHT_PX);
  }
  return offsets;
}

export type TranscriptListProps = {
  entries: TranscriptEntry[];
};

export function TranscriptList({ entries }: TranscriptListProps) {
  const listRef = useRef<HTMLOListElement | null>(null);
  // Measured row heights by entry key; estimates fill in until a row has rendered.
  const heightsRef = useRef(new Map<string, number>());
  // Keys already shown at least once — only genuinely new entries animate in, and
  // a virtualization re-mount of an old row never re-animates. `null` until the
  // first commit so the initial render is never treated as "new".
  const seenKeysRef = useRef<Set<string> | null>(null);
  const [viewport, setViewport] = useState({ scrollTop: 0, height: 0 });
  const [following, setFollowing] = useState(true);
  // Bumped when a measurement refines the height cache, to re-derive the window.
  const [, setMeasureVersion] = useState(0);

  const rowHeights = entries.map(
    (entry) => heightsRef.current.get(entry.key) ?? ESTIMATED_ROW_HEIGHT_PX
  );
  const range = computeVirtualWindow(rowHeights, viewport.scrollTop, viewport.height);
  const visible = entries.slice(range.start, range.end);
  const seenKeys = seenKeysRef.current;

  const handleScroll = () => {
    const list = listRef.current;
    if (list === null) {
      return;
    }
    setViewport({ scrollTop: list.scrollTop, height: list.clientHeight });
    const distanceFromBottom = list.scrollHeight - list.scrollTop - list.clientHeight;
    setFollowing(distanceFromBottom <= FOLLOW_THRESHOLD_PX);
  };

  const jumpToLatest = () => {
    const list = listRef.current;
    if (list === null) {
      return;
    }
    list.scrollTop = list.scrollHeight;
    setFollowing(true);
  };

  // Track the viewport size (the bounded panel region can resize with the page).
  useEffect(() => {
    const list = listRef.current;
    if (list === null) {
      return;
    }
    const observer = new ResizeObserver(() => {
      setViewport({ scrollTop: list.scrollTop, height: list.clientHeight });
    });
    observer.observe(list);
    return () => {
      observer.disconnect();
    };
  }, []);

  // Measure the rendered rows and refine the height cache; re-render once when a
  // measurement actually changed (the >0.5px guard prevents update loops).
  useEffect(() => {
    const list = listRef.current;
    if (list === null) {
      return;
    }
    let changed = false;
    for (const row of list.querySelectorAll<HTMLElement>('[data-entry-key]')) {
      const key = row.getAttribute('data-entry-key');
      if (key === null) {
        continue;
      }
      const height = row.offsetHeight;
      if (Math.abs((heightsRef.current.get(key) ?? -1) - height) > 0.5) {
        heightsRef.current.set(key, height);
        changed = true;
      }
    }
    if (changed) {
      setMeasureVersion((version) => version + 1);
    }
  });

  // Follow the tail: keep the newest entry in view unless the operator scrolled up.
  useEffect(() => {
    const list = listRef.current;
    if (list === null || !following) {
      return;
    }
    list.scrollTop = list.scrollHeight;
  });

  // Record which keys have been shown, so only later arrivals animate in.
  useEffect(() => {
    const seen = seenKeysRef.current ?? new Set<string>();
    seenKeysRef.current = seen;
    for (const entry of entries) {
      seen.add(entry.key);
    }
  }, [entries]);

  return (
    <div className="relative min-h-0 flex-1">
      <ol
        className="h-full overflow-y-auto"
        data-testid="transcript-scroll"
        onScroll={handleScroll}
        ref={listRef}
      >
        {range.topPad > 0 ? <li aria-hidden style={{ height: range.topPad }} /> : null}
        {visible.map((entry) => (
          <TranscriptEntryRow
            entry={entry}
            isNew={seenKeys !== null && !seenKeys.has(entry.key)}
            key={entry.key}
          />
        ))}
        {range.bottomPad > 0 ? <li aria-hidden style={{ height: range.bottomPad }} /> : null}
      </ol>
      {following ? null : (
        <Button
          className="absolute right-3 bottom-3 h-7 px-3 text-xs shadow-md"
          onClick={jumpToLatest}
          type="button"
          variant="outline"
        >
          Jump to latest
        </Button>
      )}
    </div>
  );
}
