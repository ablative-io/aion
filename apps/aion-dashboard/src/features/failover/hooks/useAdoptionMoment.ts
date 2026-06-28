import { useEffect, useRef, useState } from 'react';

import type { NodeLiveness } from './useNodeLiveness';

/**
 * Derive the shard-adoption moment by inference (DEMO-VIEW-PLAN §2.1 beat 3): there is
 * no structured adoption event on the wire, so we infer it from two signals that DO
 * exist — the owner node going `dark` (liveness) AND a survivor's `ActivityCompleted`
 * tally resuming (the count climbing again after the kill).
 *
 * The adoption instant is the wall-clock moment the tally first advances *after* the
 * owner went dark. Because that exact instant is inferred rather than logged, we expose
 * a `degraded` flag so the UI is honest ("adoption inferred — instant approximate")
 * rather than presenting a fabricated-precise timestamp. No silent assumptions: until
 * both conditions hold, `adopted` is false and there is no instant.
 */

export type AdoptionMoment = {
  /** True once both conditions (owner dark + tally resumed) have been observed. */
  adopted: boolean;
  /** Epoch ms when adoption was inferred; null until adopted. */
  instant: number | null;
  /** ms elapsed between the owner going dark and the tally resuming; null until adopted. */
  elapsedMs: number | null;
  /**
   * True when adoption is only inferable (the normal case — no log-tail label), so the
   * instant is approximate. Distinguishes "we inferred this" from a precise structured beat.
   */
  degraded: boolean;
};

export type UseAdoptionMomentOptions = {
  /** Liveness of the owner node (the kill target). */
  ownerLiveness: NodeLiveness | undefined;
  /** Current exactly-once completed tally from the fan-out hook. */
  completedCount: number;
  /** Optional clock injection for tests. */
  now?: () => number;
};

const IDLE: AdoptionMoment = {
  adopted: false,
  instant: null,
  elapsedMs: null,
  degraded: true,
};

export function useAdoptionMoment({
  ownerLiveness,
  completedCount,
  now = () => Date.now(),
}: UseAdoptionMomentOptions): AdoptionMoment {
  const [moment, setMoment] = useState<AdoptionMoment>(IDLE);

  // When the owner went dark, and the tally as-of that instant (to detect a resume).
  const darkAt = useRef<number | null>(null);
  const tallyAtDark = useRef<number | null>(null);

  const ownerState = ownerLiveness?.state;

  useEffect(() => {
    if (ownerState === 'dark') {
      if (darkAt.current === null) {
        darkAt.current = now();
        tallyAtDark.current = completedCount;
      }
      return;
    }

    if (ownerState === 'live') {
      // Owner recovered (or never died) — reset the inference window so a later kill
      // is detected cleanly, but keep an already-recorded adoption moment.
      darkAt.current = null;
      tallyAtDark.current = null;
    }
  }, [ownerState, completedCount, now]);

  useEffect(() => {
    if (moment.adopted) {
      return;
    }

    const wentDarkAt = darkAt.current;
    const baseline = tallyAtDark.current;

    if (wentDarkAt === null || baseline === null) {
      return;
    }

    // Adoption inferred the first time the tally advances after the owner went dark:
    // some survivor has picked up the shard and is completing activities again.
    if (completedCount > baseline) {
      const instant = now();
      setMoment({
        adopted: true,
        instant,
        elapsedMs: instant - wentDarkAt,
        degraded: true,
      });
    }
  }, [completedCount, moment.adopted, now]);

  return moment;
}
