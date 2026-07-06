import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useSearchParams } from 'react-router';

/**
 * URL-as-state for the detail surface's selection (PART 3).
 *
 * `selectedSequence` (the inspected timeline entry) and `mode` (swimlane ⇄ list)
 * are REAL navigation state: they live in the query string and each change PUSHES
 * a history entry, so browser back/forward walks the operator's selections and a
 * link/refresh restores exactly what they were looking at.
 *
 * `scrubSeq` is different: a scrub drag emits a value per tick, so pushing each one
 * would make back/forward a tour of every intermediate frame. It is therefore kept
 * as immediate LOCAL state (the slider stays responsive) and mirrored to the URL
 * with a DEBOUNCED REPLACE — the current history entry's URL is updated in place,
 * never pushed. The debounce timer is cleared on unmount so no write lands after
 * teardown.
 */
export type WorkflowViewMode = 'swimlane' | 'list';

const SEQ_PARAM = 'seq';
const MODE_PARAM = 'mode';
const SCRUB_PARAM = 'scrub';
const SCRUB_DEBOUNCE_MS = 150;

export type WorkflowSelectionParams = {
  selectedSequence: number | null;
  setSelectedSequence: (sequence: number | null) => void;
  mode: WorkflowViewMode;
  setMode: (mode: WorkflowViewMode) => void;
  scrubSeq: number | null;
  setScrub: (scrubSeq: number | null) => void;
};

/** Parse a non-negative integer query value; anything else is "unset" (`null`). */
function parseSequence(value: string | null): number | null {
  if (value === null) {
    return null;
  }
  const parsed = Number(value);
  return Number.isInteger(parsed) && parsed >= 0 ? parsed : null;
}

function parseMode(value: string | null): WorkflowViewMode {
  return value === 'list' ? 'list' : 'swimlane';
}

export function useWorkflowSelectionParams(): WorkflowSelectionParams {
  const [searchParams, setSearchParams] = useSearchParams();
  const selectedSequence = parseSequence(searchParams.get(SEQ_PARAM));
  const mode = parseMode(searchParams.get(MODE_PARAM));

  const setSelectedSequence = useCallback(
    (sequence: number | null) => {
      setSearchParams(
        (previous) => {
          const next = new URLSearchParams(previous);
          if (sequence === null) {
            next.delete(SEQ_PARAM);
          } else {
            next.set(SEQ_PARAM, String(sequence));
          }
          return next;
        },
        { replace: false }
      );
    },
    [setSearchParams]
  );

  const setMode = useCallback(
    (nextMode: WorkflowViewMode) => {
      setSearchParams(
        (previous) => {
          const next = new URLSearchParams(previous);
          // Swimlane is the default, so a clean URL omits the param entirely.
          if (nextMode === 'swimlane') {
            next.delete(MODE_PARAM);
          } else {
            next.set(MODE_PARAM, nextMode);
          }
          return next;
        },
        { replace: false }
      );
    },
    [setSearchParams]
  );

  // Scrub is local-authoritative for immediate slider feedback; the URL is a
  // debounced, replace-only mirror. It is seeded from the URL once on mount (from
  // the router's params, never `window`, so it is safe under SSR).
  const [scrubSeq, setScrubLocal] = useState<number | null>(() =>
    parseSequence(searchParams.get(SCRUB_PARAM))
  );
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const setScrub = useCallback(
    (nextScrub: number | null) => {
      setScrubLocal(nextScrub);
      if (debounceRef.current !== null) {
        clearTimeout(debounceRef.current);
      }
      debounceRef.current = setTimeout(() => {
        setSearchParams(
          (previous) => {
            const next = new URLSearchParams(previous);
            if (nextScrub === null) {
              next.delete(SCRUB_PARAM);
            } else {
              next.set(SCRUB_PARAM, String(nextScrub));
            }
            return next;
          },
          { replace: true }
        );
      }, SCRUB_DEBOUNCE_MS);
    },
    [setSearchParams]
  );

  useEffect(
    () => () => {
      if (debounceRef.current !== null) {
        clearTimeout(debounceRef.current);
      }
    },
    []
  );

  return useMemo(
    () => ({ selectedSequence, setSelectedSequence, mode, setMode, scrubSeq, setScrub }),
    [selectedSequence, setSelectedSequence, mode, setMode, scrubSeq, setScrub]
  );
}
