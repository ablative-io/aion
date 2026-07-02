// The escalation send-button state machine (the UI face of #200): while a run
// is streaming the send button *is* the interrupt control, and pressing it
// again within the decay window escalates interrupt → shutdown → kill. Left
// idle, it steps back down one level per window. Pure + injectable-clock-free
// so it is directly testable with fake timers.

export type EscalationLevel = 'interrupt' | 'shutdown' | 'kill';

export const ESCALATION_ORDER: readonly EscalationLevel[] = ['interrupt', 'shutdown', 'kill'];

export const ESCALATION_DECAY_MS = 3000;

/** The level armed after firing the given one (kill is the ceiling). */
export function escalate(level: EscalationLevel): EscalationLevel {
  if (level === 'interrupt') return 'shutdown';
  return 'kill';
}

/** One step back toward the resting level (interrupt is the floor). */
export function decay(level: EscalationLevel): EscalationLevel {
  if (level === 'kill') return 'shutdown';
  return 'interrupt';
}

export type EscalationMachine = {
  /** The level the *next* press will fire. */
  level: () => EscalationLevel;
  /** Fire the armed level; arms the next one and (re)starts the decay clock. */
  press: () => EscalationLevel;
  /** Snap back to rest (e.g. the run stopped streaming). */
  reset: () => void;
  dispose: () => void;
};

export function createEscalationMachine(
  onLevelChange?: (level: EscalationLevel) => void,
  decayMs: number = ESCALATION_DECAY_MS
): EscalationMachine {
  let level: EscalationLevel = 'interrupt';
  let timer: ReturnType<typeof setTimeout> | null = null;

  const clearTimer = () => {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  };

  const setLevel = (next: EscalationLevel) => {
    if (next === level) return;
    level = next;
    onLevelChange?.(next);
  };

  const armDecay = () => {
    clearTimer();
    if (level === 'interrupt') return;
    timer = setTimeout(() => {
      setLevel(decay(level));
      armDecay();
    }, decayMs);
  };

  return {
    level: () => level,
    press: () => {
      const fired = level;
      setLevel(escalate(level));
      armDecay();
      return fired;
    },
    reset: () => {
      clearTimer();
      setLevel('interrupt');
    },
    dispose: clearTimer,
  };
}
