import { useLayoutEffect, useRef, useState } from 'react';

import { Button } from '@/components/ui';

import type { TranscriptEntry } from '../hooks/useTranscript';
import { TranscriptEntryRow } from './TranscriptEventRow';

/**
 * Bounded transcript list (NOI-7).
 *
 * The list owns a SELF-BOUNDING scroll region. The `<ol>` is a `min-h-0 flex-1`
 * item of the (flex-column) wrapper, so inside the clamped panel it sizes to the
 * space REMAINING under the panel header — never past the panel edge, which would
 * clip its own bottom rows behind the panel's overflow clip. `max-h-[70vh]` stays
 * on the `<ol>` as a backstop so the scroller stays bounded even in a host that
 * never clamps it (the AppShell chain is content-sized, which previously let the
 * page grow unboundedly). No percentage heights anywhere — flex sizing only, so
 * there is no definite-height-ancestor dependency to go bistable.
 * Every entry renders as a plain `<li>` — no virtualization. That is
 * deliberate and safe here: the hook folds token deltas into ONE growing stream
 * row and coalesces repeated progress notes, so the entry count stays moderate;
 * and each row clamps its own oversized payload (see {@link TranscriptEntryRow}),
 * so a giant `JSON.stringify` body can never blow out a row. Rows are memoized on
 * their (immutable) entry reference, so a streamed token only re-renders the one
 * row whose text actually grew.
 *
 * New entries flow in at the bottom with a subtle entrance and the list follows
 * the tail — unless the operator has scrolled up, in which case it stays put and
 * offers a "Jump to latest" affordance instead of yanking them down.
 */

/** How close (px) to the bottom still counts as "following the tail". */
const FOLLOW_THRESHOLD_PX = 48;

export type TranscriptListProps = {
  entries: TranscriptEntry[];
};

export function TranscriptList({ entries }: TranscriptListProps) {
  const listRef = useRef<HTMLOListElement | null>(null);
  // Keys already shown at least once — only genuinely new entries animate in.
  // `null` until the first commit so the initial render is never treated as "new".
  const seenKeysRef = useRef<Set<string> | null>(null);
  const [following, setFollowing] = useState(true);

  const seenKeys = seenKeysRef.current;

  const handleScroll = () => {
    const list = listRef.current;
    if (list === null) {
      return;
    }
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

  // Follow the tail: whenever the entries change (the fold returns a NEW array for
  // every real event, including the last row growing in place as tokens coalesce)
  // and the operator is still at the bottom, pin the view to the latest. Guarded by
  // `following`, so a reader who has scrolled up is never yanked down. Layout effect
  // so the re-pin happens before paint (no visible jump). A no-op duplicate event
  // returns the SAME array reference, so this does not fire on those.
  useLayoutEffect(() => {
    const list = listRef.current;
    // Reading `entries` (its length) is what ties this re-pin to every folded update:
    // the fold hands back a NEW array reference for each real event — including the
    // last row growing in place as tokens coalesce — so the tail stays glued.
    if (!following || list === null || entries.length === 0) {
      return;
    }
    list.scrollTop = list.scrollHeight;
  }, [entries, following]);

  // Record which keys have been shown, so only later arrivals animate in.
  useLayoutEffect(() => {
    const seen = seenKeysRef.current ?? new Set<string>();
    seenKeysRef.current = seen;
    for (const entry of entries) {
      seen.add(entry.key);
    }
  }, [entries]);

  return (
    <div className="relative flex min-h-0 flex-1 flex-col">
      <ol
        className="max-h-[70vh] min-h-0 flex-1 overflow-y-auto"
        data-testid="transcript-scroll"
        onScroll={handleScroll}
        ref={listRef}
      >
        {entries.map((entry) => (
          <TranscriptEntryRow
            entry={entry}
            isNew={seenKeys !== null && !seenKeys.has(entry.key)}
            key={entry.key}
          />
        ))}
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
