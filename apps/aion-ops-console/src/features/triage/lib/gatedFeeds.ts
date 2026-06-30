import type { IncidentKind } from './incidents';

/**
 * Server-gated incident classes (plan §4.4 / §8). These need topology / outbox
 * subscription kinds that are NOT on the pinned wire today — the WebSocket
 * `AionEventSubscriptionFilter` is `per_workflow | filtered | firehose` only.
 *
 * Rather than fabricate counts (house rule: never fake numbers), the triage view
 * renders an explicit "awaiting server support" row per gated class so an
 * operator can SEE that the feed exists in the product's intent but is not yet
 * wired — a structural placeholder, never a silent gap and never a fake number.
 */
export type GatedFeed = {
  kind: Extract<
    IncidentKind,
    'dead-worker' | 'outbox-failed' | 'fenced-rejection' | 'shard-adoption'
  >;
  /** What this class of incident reports, in operator language. */
  label: string;
  /** Why it is not live yet, stated plainly. */
  reason: string;
};

/**
 * The gated feeds, ordered by triage priority (dead worker first). Live failover
 * incidents (shard adoption, fencing) are already covered by the dedicated
 * `/failover` view; the shard-adoption card here links INTO that view rather
 * than duplicating its feed.
 */
export const GATED_FEEDS: readonly GatedFeed[] = [
  {
    kind: 'dead-worker',
    label: 'Dead workers',
    reason: 'needs a server worker-liveness feed (no subscription kind on the wire yet)',
  },
  {
    kind: 'outbox-failed',
    label: 'Failed outbox rows',
    reason: 'needs a server outbox feed (no subscription kind on the wire yet)',
  },
  {
    kind: 'fenced-rejection',
    label: 'Fenced rejections',
    reason: 'needs a server topology feed (no subscription kind on the wire yet)',
  },
  {
    kind: 'shard-adoption',
    label: 'Shard adoptions',
    reason: 'live in the failover view; promote here when a topology feed is pinned',
  },
] as const;
