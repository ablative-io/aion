import type { AionSocketError, ConnectionStatus } from '@/lib/api';

/**
 * Always-on cluster provenance line (ADR-016). Every cluster surface must say
 * WHICH node it is reading from, the last `cluster_seq` it applied, and surface a
 * "viewing a survivor" banner after an own-read failover repoint. A silently
 * stale view is a defect, not graceful degradation — so a non-`connected` socket
 * and a typed {@link AionSocketError} are rendered as visible state, never
 * swallowed.
 */

export type ClusterProvenanceProps = {
  /** The reading node (own-read target); null when none is live. */
  readingNodeLabel: string | null;
  /** The cluster's self-reported node identity from the snapshot. */
  selfNode: string | null;
  /** Highest `cluster_seq` applied so far. */
  lastSeq: number;
  /** ISO-8601 instant of the most recently applied frame; null pre-prime. */
  lastObservedAt: string | null;
  /** True once a priming snapshot has been applied. */
  primed: boolean;
  /** Live-socket connection status. */
  status: ConnectionStatus;
  /** True when reads have failed over to a survivor. */
  failedOver: boolean;
  /** Typed live-socket error, or null. */
  error: AionSocketError | null;
};

// "connected" describes the SOCKET, not data currency. A cluster event stream is
// legitimately silent while the cluster is healthy and stable (WS3 emits only on
// real state changes — there is no keepalive yet), so we must not imply
// continuous freshness with a word like "live". Distinguishing a healthy-quiet
// socket from a wedged one requires server keepalive frames (ADR-027); until that
// lands the timestamp below is labelled "last change", which is honest for both.
const STATUS_LABEL: Record<ConnectionStatus, string> = {
  connected: 'connected',
  'resynced-with-possible-gap': 'possible gap',
  reconnecting: 'reconnecting',
  disconnected: 'disconnected',
};

const STATUS_COLOR: Record<ConnectionStatus, string> = {
  connected: 'var(--status-live)',
  'resynced-with-possible-gap': 'var(--status-warning)',
  reconnecting: 'var(--text-muted)',
  disconnected: 'var(--status-danger)',
};

export function ClusterProvenance({
  readingNodeLabel,
  selfNode,
  lastSeq,
  lastObservedAt,
  primed,
  status,
  failedOver,
  error,
}: ClusterProvenanceProps) {
  return (
    <div className="flex flex-col gap-1 rounded-md border border-border bg-card px-3 py-2 font-mono text-xs">
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-secondary-foreground">
        <span className="flex items-center gap-1.5">
          <span
            aria-hidden="true"
            className="inline-block size-2 rounded-full"
            style={{ backgroundColor: STATUS_COLOR[status] }}
          />
          cluster stream: {STATUS_LABEL[status]}
        </span>
        <span>reading: {readingNodeLabel ?? '—'}</span>
        <span>self: {selfNode ?? (primed ? 'standalone' : '…')}</span>
        <span>cluster_seq: {primed ? lastSeq : '—'}</span>
        {lastObservedAt !== null ? (
          <span title={lastObservedAt}>last change {formatInstant(lastObservedAt)}</span>
        ) : null}
      </div>

      {failedOver ? (
        <p className="font-semibold text-primary">
          ⚠ viewing a survivor — reads have failed over from the configured owner.
        </p>
      ) : null}

      {error !== null ? (
        <p className="text-destructive">cluster stream error: {error.message}</p>
      ) : null}

      {!primed && error === null ? (
        <p className="text-muted-foreground">awaiting cluster snapshot…</p>
      ) : null}
    </div>
  );
}

function formatInstant(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) {
    return iso;
  }
  return date.toLocaleTimeString();
}
