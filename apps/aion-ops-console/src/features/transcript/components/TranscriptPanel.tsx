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
  reconnecting: 'Reconnecting',
};

const STATUS_STYLES: Record<ConnectionStatus, string> = {
  connected: 'border-emerald-500/40 bg-emerald-500/10 text-emerald-500',
  disconnected: 'border-destructive/40 bg-destructive/10 text-destructive',
  reconnecting: 'border-amber-500/40 bg-amber-500/10 text-amber-500',
};

export type TranscriptPanelProps = {
  /** The attempt target to stream; `null` renders the "select an attempt" empty state. */
  target: TranscriptTarget | null;
};

/** The data-bound panel: owns the transcript subscription for `target`. */
export function TranscriptPanel({ target }: TranscriptPanelProps) {
  const { entries, status, socketError } = useTranscript({ target });

  return (
    <TranscriptPanelContent
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
};

/** Presentational panel body, driven by already-resolved props (unit-testable). */
export function TranscriptPanelContent({
  entries,
  status,
  socketError,
  hasTarget,
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
      className="flex max-h-[70vh] min-h-0 flex-col gap-3 rounded-xl border border-[var(--border-default)] p-3"
      data-testid="transcript-panel"
    >
      <header className="flex items-center justify-between gap-2">
        <h2 className="font-semibold text-[var(--text-primary)] text-sm">Agent transcript</h2>
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
          className="rounded-lg border border-amber-500/40 bg-amber-500/10 p-2 text-amber-600 text-xs"
          data-testid="transcript-socket-error"
          role="status"
        >
          {socketError.message}
        </p>
      )}
      {entries.length === 0 ? (
        <p className="py-6 text-center text-[var(--text-muted)] text-sm">
          No transcript events yet.
        </p>
      ) : (
        <TranscriptList entries={entries} />
      )}
    </section>
  );
}
