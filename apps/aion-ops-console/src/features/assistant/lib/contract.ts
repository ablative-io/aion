import type { JsonRecord } from '@/lib/api';
import type { WorkflowFilter } from '@/types';

/**
 * The assistant workflow contract (built in the parallel backend track as a
 * Gleam package). One place pins the type + signal names + input shape so the
 * UI and the workflow can only drift in one visible spot.
 */
export const ASSISTANT_WORKFLOW_TYPE = 'assistant';

/**
 * The ONE control signal the session listens on — the engine's selective
 * receive parks on exactly this name, discriminating in the payload:
 * `{ message }` starts the next round, `{ end: true }` ends the session
 * (OperatorEnded). There is deliberately NO separate end signal.
 */
export const ASSISTANT_CONTINUE_SIGNAL = 'assistant_continue';

/** The end-of-session payload for {@link ASSISTANT_CONTINUE_SIGNAL}. */
export function assistantEndPayload(): JsonRecord {
  return { end: true };
}

/** The `POST /workflows/start` input for an assistant session. */
export type AssistantSessionInput = {
  objective: string;
  repo_path: string;
};

export function assistantStartInput(objective: string, repoPath: string): JsonRecord {
  // The contract shape carries repo_path unconditionally; a blank entry travels
  // as the empty string (the workflow treats it as "no repo pinned").
  const input: AssistantSessionInput = { objective, repo_path: repoPath };
  return input;
}

/** The session-list filter: exactly the assistant workflow type, any status. */
export function assistantSessionsFilter(): WorkflowFilter {
  return {
    workflow_type: ASSISTANT_WORKFLOW_TYPE,
    status: null,
    started_after: null,
    started_before: null,
    parent: null,
  };
}
