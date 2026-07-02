import { cn } from '@/lib/utils';
import type { NodeLivenessState } from '../hooks/useNodeLiveness';
import type { ClusterNode } from '../lib/clusterConfig';

export type NodeCardProps = {
  node: ClusterNode;
  /** Liveness from the per-node /health/live poll. */
  state: NodeLivenessState;
  /** Visible reason a probe failed; null when live/unknown. */
  livenessError: string | null;
  /** Connected workers from /metrics; null hides the row (never zeroed — §1.1). */
  workers: number | null;
  /** Visible reason metrics are missing; null when present. */
  metricsError: string | null;
  /**
   * True when THIS node currently owns the demo shard (shard 0). The owner marker
   * MOVES here on adoption: pre-kill it sits on the configured owner, post-kill it
   * moves to the adopter. The movement IS the failover story rendered as shape.
   */
  ownsActiveShard: boolean;
  /** True before the kill — tags the seeded owner as "pre-kill" configured state. */
  preKill: boolean;
};

const DOT_STYLE: Record<NodeLivenessState, string> = {
  live: 'bg-live',
  dark: 'bg-danger',
  unknown: 'border border-muted-foreground bg-transparent',
};

const LABEL: Record<NodeLivenessState, string> = {
  live: 'LIVE',
  dark: 'DARK',
  unknown: 'UNKNOWN',
};

const LABEL_COLOR: Record<NodeLivenessState, string> = {
  live: 'var(--status-live)',
  dark: 'var(--status-danger)',
  unknown: 'var(--text-muted)',
};

/**
 * One cluster node: liveness dot (live / dark / unknown), shard row carrying the
 * owner marker, and a worker count. Honesty under pressure (§1.1): an unpolled node
 * shows a hollow `unknown` dot and "no health response", never a silent LIVE; a node
 * with no metrics hides its worker row rather than lying with a 0.
 */
export function NodeCard({
  node,
  state,
  livenessError,
  workers,
  metricsError,
  ownsActiveShard,
  preKill,
}: NodeCardProps) {
  const isDark = state === 'dark';

  return (
    <div
      className={cn(
        'flex flex-col gap-3 rounded-md border p-4 transition-colors',
        isDark ? 'border-destructive/40 bg-destructive/5 opacity-70' : 'border-border bg-card'
      )}
      data-liveness={state}
      data-node-index={node.index}
    >
      <div className="flex items-center justify-between">
        <span className="font-mono text-foreground text-sm">{node.label}</span>
        <span className="font-mono text-muted-foreground text-xs">:{node.httpPort}</span>
      </div>

      <div className="flex items-center gap-2">
        <span aria-hidden="true" className={cn('size-2.5 rounded-full', DOT_STYLE[state])} />
        <span className="font-semibold text-xs tracking-wide" style={{ color: LABEL_COLOR[state] }}>
          {LABEL[state]}
        </span>
      </div>

      {state === 'unknown' ? (
        <p className="text-muted-foreground text-xs">no health response</p>
      ) : null}
      {isDark && livenessError !== null ? (
        <p className="text-destructive text-xs">{livenessError}</p>
      ) : null}

      <div className="flex items-center gap-2 border-border border-t pt-2">
        <span className="font-mono text-secondary-foreground text-sm">shard {node.ownedShard}</span>
        {ownsActiveShard ? (
          <span
            className="font-mono text-xs"
            data-owner-marker="true"
            style={{ color: 'var(--status-live)' }}
          >
            ◀ owner
          </span>
        ) : null}
        {preKill && node.ownedShard === 0 && !ownsActiveShard ? (
          <span className="font-mono text-muted-foreground text-xs">pre-kill</span>
        ) : null}
      </div>

      {workers !== null ? (
        <span className="font-mono text-secondary-foreground text-xs">wk: {workers}</span>
      ) : (
        <span className="font-mono text-muted-foreground text-xs" title={metricsError ?? ''}>
          wk: —
        </span>
      )}
    </div>
  );
}
