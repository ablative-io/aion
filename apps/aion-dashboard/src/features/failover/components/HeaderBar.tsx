import { ConnectionIndicatorContent } from '@/features/live-feed/components/ConnectionIndicator';
import type { ConnectionStatus } from '@/lib/api';
import type { Namespace } from '@/types';

export type HeaderBarProps = {
  /** WS connection status for the global indicator. */
  status: ConnectionStatus;
  namespace: Namespace;
  /** Label of the node the view is currently reading from (own-read failover). */
  activeNodeLabel: string | null;
  /** True when reads have failed over off the preferred target to a survivor. */
  failedOver: boolean;
};

/**
 * Top bar: title + global connection indicator + namespace + the active read
 * target. The active-node label is load-bearing during the demo — when node 0 is
 * killed, the own-read failover repoints the view at a survivor, and this row is
 * where that switch becomes visible (rather than silently re-targeting).
 */
export function HeaderBar({ status, namespace, activeNodeLabel, failedOver }: HeaderBarProps) {
  return (
    <header className="flex flex-col gap-4 border-[var(--border-default)] border-b pb-4 md:flex-row md:items-center md:justify-between">
      <div className="space-y-1">
        <p className="font-medium text-[0.7rem] text-[var(--text-muted)] uppercase tracking-[0.3em]">
          AION
        </p>
        <h1 className="font-semibold text-2xl text-[var(--text-primary)] tracking-tight">
          FAILOVER
        </h1>
      </div>
      <div className="flex flex-wrap items-end gap-4">
        <div className="flex flex-col gap-1">
          <span className="font-medium text-[var(--text-muted)] text-xs">Read target</span>
          <span
            className="font-mono text-sm"
            data-failed-over={failedOver}
            style={{ color: failedOver ? 'var(--destructive)' : 'var(--text-secondary)' }}
          >
            {activeNodeLabel ?? 'no live node'}
            {failedOver ? ' (failed over)' : ''}
          </span>
        </div>
        <div className="flex flex-col gap-1">
          <span className="font-medium text-[var(--text-muted)] text-xs">Namespace</span>
          <span className="font-mono text-[var(--text-secondary)] text-sm">ns: {namespace}</span>
        </div>
        <ConnectionIndicatorContent status={status} />
      </div>
    </header>
  );
}
