import type { WorkflowSummary } from '@/types';

/**
 * Triage incident model (plan §4.4 / slice S7).
 *
 * An incident is a ranked, actionable thing-that-is-wrong, derived from server
 * truth. Phase-1 derives only WORKFLOW-failure incidents (failed + stuck-running)
 * from the list query, because that is the only signal on the pinned wire. The
 * worker / outbox / node-shard / fenced incident classes require server feeds
 * (topology / outbox subscription kinds) that are NOT in the WebSocket contract
 * today (`AionEventSubscriptionFilter` is per_workflow | filtered | firehose
 * only). Those classes are spec'd here as a closed union so the card surface and
 * the ranking are shaped for them, but the hook emits none of them until the
 * server promotes the feeds — it never fabricates one.
 */

/** Incident kinds. `workflow-failure` and `workflow-stuck` ship now; the rest are server-gated. */
export type IncidentKind =
  | 'workflow-failure'
  | 'workflow-stuck'
  | 'dead-worker'
  | 'outbox-failed'
  | 'fenced-rejection'
  | 'shard-adoption';

/** Where a card sends the operator. Honest about what is and is not reachable. */
export type IncidentTarget =
  | { kind: 'workflow-detail'; workflowId: string; seq?: number }
  | { kind: 'failover' };

export type Incident = {
  /** Stable identity for keying + dedup across live refetches. */
  id: string;
  kind: IncidentKind;
  /** One-line headline an on-call engineer reads in under five seconds. */
  title: string;
  /** Supporting detail (workflow type, age, error class). */
  detail: string;
  /** ISO-8601 timestamp the incident is anchored to (start / failure time). */
  at: string | null;
  /** Higher = more urgent; the list sorts descending by this. */
  rank: number;
  /** Where the single inline action takes the operator. */
  target: IncidentTarget;
  /** Label for the single inline action. */
  actionLabel: string;
};

/**
 * A workflow that has been Running longer than this is treated as "stuck" for
 * triage ranking. This is an honest age heuristic, not a server-reported stall:
 * the card copy says "running > Nm" so the operator knows it is a heuristic, not
 * a verdict.
 */
export const STUCK_RUNNING_THRESHOLD_MS = 15 * 60 * 1000;

const FAILURE_RANK = 100;
const STUCK_RANK_BASE = 50;

/**
 * Rank-derive workflow incidents from a page of summaries. Pure: same inputs →
 * same incidents, no clock reads except the supplied `now`. Failed workflows
 * outrank stuck-running ones; within a class, more recent first.
 */
export function deriveWorkflowIncidents(
  workflows: readonly WorkflowSummary[],
  now: number,
  stuckThresholdMs = STUCK_RUNNING_THRESHOLD_MS
): Incident[] {
  const incidents: Incident[] = [];

  for (const workflow of workflows) {
    const failure = failureIncident(workflow);
    if (failure !== null) {
      incidents.push(failure);
      continue;
    }

    const stuck = stuckIncident(workflow, now, stuckThresholdMs);
    if (stuck !== null) {
      incidents.push(stuck);
    }
  }

  return sortIncidents(incidents);
}

function failureIncident(workflow: WorkflowSummary): Incident | null {
  if (workflow.status !== 'Failed' && workflow.status !== 'TimedOut') {
    return null;
  }

  const failed = workflow.status === 'Failed';
  const anchor = workflow.ended_at ?? workflow.started_at;

  return {
    id: `wf-fail:${workflow.workflow_id}`,
    kind: 'workflow-failure',
    title: failed ? `${workflow.workflow_type} failed` : `${workflow.workflow_type} timed out`,
    detail: `workflow ${workflow.workflow_id}`,
    at: anchor,
    rank: FAILURE_RANK,
    target: { kind: 'workflow-detail', workflowId: workflow.workflow_id },
    actionLabel: 'Open swimlane',
  };
}

function stuckIncident(
  workflow: WorkflowSummary,
  now: number,
  stuckThresholdMs: number
): Incident | null {
  if (workflow.status !== 'Running') {
    return null;
  }

  const startedMs = Date.parse(workflow.started_at);
  if (Number.isNaN(startedMs)) {
    return null;
  }

  const ageMs = now - startedMs;
  if (ageMs < stuckThresholdMs) {
    return null;
  }

  return {
    id: `wf-stuck:${workflow.workflow_id}`,
    kind: 'workflow-stuck',
    title: `${workflow.workflow_type} running > ${Math.floor(ageMs / 60000)}m`,
    detail: `workflow ${workflow.workflow_id} (age heuristic, not a server stall report)`,
    at: workflow.started_at,
    rank: STUCK_RANK_BASE + ageRank(ageMs),
    target: { kind: 'workflow-detail', workflowId: workflow.workflow_id },
    actionLabel: 'Open swimlane',
  };
}

/** Older stuck workflows rank slightly higher (capped) so the list does not invert wildly. */
function ageRank(ageMs: number): number {
  return Math.min(40, Math.floor(ageMs / (5 * 60 * 1000)));
}

export function sortIncidents(incidents: readonly Incident[]): Incident[] {
  return [...incidents].sort((a, b) => {
    if (b.rank !== a.rank) {
      return b.rank - a.rank;
    }
    return compareAtDescending(a.at, b.at);
  });
}

function compareAtDescending(a: string | null, b: string | null): number {
  const aMs = a === null ? 0 : Date.parse(a);
  const bMs = b === null ? 0 : Date.parse(b);
  const safeA = Number.isNaN(aMs) ? 0 : aMs;
  const safeB = Number.isNaN(bMs) ? 0 : bMs;
  return safeB - safeA;
}
