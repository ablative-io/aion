import { EmptyState } from '@/components/EmptyState';
import { Badge } from '@/components/ui';
import type { AionSocketError, ConnectionStatus } from '@/lib/api';
import type { TranscriptTarget } from '@/lib/api/transcript-stream';
import { cn } from '@/lib/utils';

import { type TranscriptEntry, useTranscript } from '../hooks/useTranscript';
import { TranscriptList } from './TranscriptList';

/**
 * Live agent-transcript panel (NOI-7).
 *
 * Subscribes to the transcript WebSocket for one `(workflow, activity, attempt)`
 * target and renders the live {@link TranscriptEntry} stream in real time. REAL
 * DATA ONLY, socket-first, NO polling. The durable `O`-keyspace tail arrives first
 * and live deltas splice on with no gap/dup (the hook resumes by `store_seq`).
 * A degraded socket is shown as visible state, never swallowed.
 */

const STATUS_LABELS: Record<ConnectionStatus, string> = {
  connected: 'Live',
  disconnected: 'Disconnected',
  'resynced-with-possible-gap': 'Live with possible gap',
  reconnecting: 'Reconnecting',
};

const STATUS_STYLES: Record<ConnectionStatus, string> = {
  connected: 'border-success/40 bg-success-glow text-success',
  disconnected: 'border-danger/40 bg-danger-glow text-danger',
  'resynced-with-possible-gap': 'border-warning/40 bg-warning-glow text-warning',
  reconnecting: 'border-warning/40 bg-warning-glow text-warning',
};

export type TranscriptPanelProps = {
  /** The attempt target to stream; `null` renders the "select an attempt" empty state. */
  target: TranscriptTarget | null;
};

/** The data-bound panel: owns the transcript subscription for `target`. */
export function TranscriptPanel({ target }: TranscriptPanelProps) {
  const { entries, status, socketError, backfillState, backfillError } = useTranscript({ target });

  return (
    <TranscriptPanelContent
      backfillError={backfillError}
      backfillState={backfillState}
      entries={entries}
      hasTarget={target !== null}
      socketError={socketError}
      status={status}
    />
  );
}

export type TranscriptPanelContentProps = {
  entries: TranscriptEntry[];
  status: ConnectionStatus;
  socketError: AionSocketError | null;
  hasTarget: boolean;
  /** REST retained-backfill failure, surfaced visibly (defaults absent). */
  backfillError?: Error | null;
  /** REST retained-backfill lifecycle (informational; defaults `ready`). */
  backfillState?: 'loading' | 'ready' | 'error';
};

/** Presentational panel body, driven by already-resolved props (unit-testable). */
export function TranscriptPanelContent({
  entries,
  status,
  socketError,
  hasTarget,
  backfillError = null,
}: TranscriptPanelContentProps) {
  if (!hasTarget) {
    return (
      <EmptyState
        description="Select a live activity attempt to watch its agent transcript."
        title="No attempt selected"
      />
    );
  }

  return (
    <section
      className="flex max-h-[70vh] min-h-0 flex-col gap-3 overflow-hidden rounded-xl border border-border p-3"
      data-testid="transcript-panel"
    >
      <header className="flex items-center justify-between gap-2">
        <h2 className="font-semibold text-foreground text-sm">Agent transcript</h2>
        <Badge
          aria-label={`Transcript ${STATUS_LABELS[status].toLowerCase()}`}
          className={cn('gap-1.5', STATUS_STYLES[status])}
          variant="outline"
        >
          {STATUS_LABELS[status]}
        </Badge>
      </header>
      {socketError === null ? null : (
        <p
          className="rounded-lg border border-warning/40 bg-warning-glow p-2 text-warning text-xs"
          data-testid="transcript-socket-error"
          role="status"
        >
          {socketError.message}
        </p>
      )}
      {backfillError === null ? null : (
        <p
          className="rounded-lg border border-warning/40 bg-warning-glow p-2 text-warning text-xs"
          data-testid="transcript-backfill-error"
          role="status"
        >
          {`Retained transcript fetch failed: ${backfillError.message} — showing the live-socket replay instead.`}
        </p>
      )}
      {entries.length === 0 ? (
        <p className="py-6 text-center text-muted-foreground text-sm">No transcript events yet.</p>
      ) : (
        <TranscriptList entries={entries} />
      )}
    </section>
  );
}
