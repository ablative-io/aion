import { Badge } from '@/components/ui';
import { cn } from '@/lib/utils';

import type { NamespaceQuota } from '../hooks/useNamespaceRegistry';

/**
 * Live per-namespace concurrency-quota badge (Control-Plane Phase 2, P2-Q3).
 *
 * Renders `in_flight / ceiling` from the server's periodic `NamespaceQuotaState`
 * push, updating live as work flows — no client poll. Until the first snapshot for
 * a namespace arrives (typically within one server cadence) the quota is `undefined`
 * and the badge shows an honest "—" placeholder rather than a fabricated zero.
 *
 * The at/over-ceiling state is called out visually: a namespace whose durable
 * Claimed count has reached (or, under shard-skew over-admit, exceeded) its ceiling
 * is throttled — its excess rows are held Pending by the keyed backpressure — so the
 * badge turns amber and is labelled "throttled" so an operator sees the ceiling bite.
 */

export type QuotaBadgeProps = {
  quota: NamespaceQuota | undefined;
};

export function QuotaBadge({ quota }: QuotaBadgeProps) {
  if (quota === undefined) {
    return (
      <Badge
        variant="outline"
        className="w-fit font-mono text-[var(--text-muted)]"
        aria-label="Quota pending first snapshot"
      >
        —
      </Badge>
    );
  }

  const atCeiling = quota.inFlight >= quota.ceiling;

  return (
    <Badge
      variant="outline"
      className={cn(
        'w-fit gap-1.5 font-mono',
        atCeiling
          ? 'border-amber-500/40 bg-amber-500/10 text-amber-500'
          : 'text-[var(--text-secondary)]'
      )}
      aria-label={
        atCeiling
          ? `Throttled: ${quota.inFlight} of ${quota.ceiling} in flight`
          : `${quota.inFlight} of ${quota.ceiling} in flight`
      }
    >
      <span>
        {quota.inFlight} / {quota.ceiling}
      </span>
      {atCeiling ? (
        <span className="font-sans font-medium text-[10px] uppercase tracking-wide">throttled</span>
      ) : null}
    </Badge>
  );
}
