import type { KitStatus } from '@/components/kit';
import type { AttemptCapabilities } from '@/lib/api';
import type { WorkflowSummary } from '@/types';

/**
 * What the chat dock's send verb means right now. Derived from REAL state — the
 * projected terminal outcome of the live-merged event history plus the live
 * intervenable-attempt enumeration — never guessed:
 *
 * - `live`: an assistant attempt is running → send = intervene InjectMessage
 *   (interrupt priority) at that attempt.
 * - `awaiting`: the workflow is open but no attempt is live → the round ended
 *   and the workflow is parked on its signal receive → send =
 *   `assistant_continue { message }`.
 * - `connecting`: the attempt enumeration hasn't answered yet — sending has no
 *   trustworthy target, so the dock isn't offered.
 * - `ended`: the workflow is terminal; the session is over.
 */
export type AssistantChatMode = 'live' | 'awaiting' | 'connecting' | 'ended';

export type AssistantModeInputs = {
  /** Projected terminal outcome exists (Completed/Failed/Cancelled/TimedOut). */
  isTerminal: boolean;
  /** The live-attempt enumeration has answered (loadState === 'ready'). */
  attemptsReady: boolean;
  liveAttemptCount: number;
};

export function deriveAssistantMode(inputs: AssistantModeInputs): AssistantChatMode {
  if (inputs.isTerminal) {
    return 'ended';
  }
  if (!inputs.attemptsReady) {
    return 'connecting';
  }
  return inputs.liveAttemptCount > 0 ? 'live' : 'awaiting';
}

/**
 * The attempt the chat targets: the NEWEST live attempt (highest activity,
 * then attempt) — assistant rounds are sequential, so the newest one is the
 * round in flight. Expects the ascending order `sortAttempts` produces.
 */
export function selectAssistantAttempt(
  sorted: readonly AttemptCapabilities[]
): AttemptCapabilities | null {
  return sorted.at(-1) ?? null;
}

/** The pill copy that makes the current send semantics obvious. */
export const MODE_PLACEHOLDER: Record<AssistantChatMode, string> = {
  live: 'Agent is working — messages interrupt…',
  awaiting: 'Agent finished — send to continue…',
  connecting: 'Connecting to session…',
  ended: 'Session ended',
};

/** The pill status dot per mode (semantic kit statuses, tokenized). */
export const MODE_STATUS: Record<AssistantChatMode, KitStatus> = {
  live: 'live',
  awaiting: 'healthy',
  connecting: 'idle',
  ended: 'idle',
};

/** Newest-first ordering for the session list (started_at descending). */
export function sortSessionsNewestFirst(sessions: readonly WorkflowSummary[]): WorkflowSummary[] {
  return [...sessions].sort((left, right) => right.started_at.localeCompare(left.started_at));
}
