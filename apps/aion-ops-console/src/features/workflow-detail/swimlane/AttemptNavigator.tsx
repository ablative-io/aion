import { useMemo, useState } from 'react';

import { EmptyState } from '@/components/EmptyState';
import type { KitStatus } from '@/components/kit';
import { StatusDot } from '@/components/kit';
import { Button } from '@/components/ui';
import {
  InterventionActions,
  InterventionComposer,
  TranscriptPanel,
  useActivityAttempts,
  useInterventionController,
} from '@/features/transcript';
import type { AttemptCapabilities } from '@/lib/api';
import type { TranscriptTarget } from '@/lib/api/transcript-stream';
import { cn } from '@/lib/utils';
import type { Namespace, WorkflowId } from '@/types';

import {
  type AttemptNavigatorRow,
  type AttemptStatus,
  attemptRowKey,
  deriveAttemptNavigator,
} from '../lib/attemptNavigator';
import type { TimelineEntry } from '../types';

export type AttemptNavigatorProps = {
  workflowId: WorkflowId;
  namespace: Namespace | null;
  /** Durable timeline (history + live) the attempt rows are derived from. */
  timeline: readonly TimelineEntry[];
};

/**
 * The DURABLE attempt navigator (PART 1). It enumerates the selectable attempts
 * from the projected timeline — durable history + live — so a terminal (failed or
 * finished) run's attempts stay selectable and their durable transcripts stay
 * cold-loadable. Every row and the current selection are keyed by the durable
 * `(activityId, attempt)` identity, never by array index.
 *
 * Selecting a row streams that attempt's transcript via {@link TranscriptPanel}
 * (the transcript stream serves the durable `O` tail first, so a FINISHED attempt
 * loads). The LIVE enumeration (`useActivityAttempts`) is retained ONLY to gate
 * intervention: the controls appear ONLY for a currently-live attempt whose
 * identity matches the selected row; a terminal run is read-only.
 *
 * Layout is a single full-width vertical stack: header (title + the non-inject
 * intervention actions + Refresh live) → attempt rows → the transcript → the
 * inject composer docked directly beneath it (the assistant page's "chat pill
 * beneath the transcript" pattern). The destructive actions live in the HEADER,
 * maximally distant from the composer's Send, so Cancel is never a slip of the
 * hand away. Both pieces share ONE {@link useInterventionController} instance —
 * one honest submit/outcome/error stream, reported next to the composer.
 */
export function AttemptNavigator({ workflowId, namespace, timeline }: AttemptNavigatorProps) {
  const rows = useMemo(() => deriveAttemptNavigator(timeline), [timeline]);
  const [pickedKey, setPickedKey] = useState<string | null>(null);
  const { attempts: liveAttempts, refresh } = useActivityAttempts({ workflowId, namespace });

  const selectedRow = resolveSelectedRow(rows, pickedKey);
  const target: TranscriptTarget | null = useMemo(() => {
    if (selectedRow === null || namespace === null) {
      return null;
    }
    return {
      namespace,
      workflowId,
      activityId: Number(selectedRow.activityId),
      attempt: selectedRow.attempt,
    };
  }, [selectedRow, namespace, workflowId]);

  // Intervention is gated on a currently-LIVE attempt whose identity EQUALS the
  // selected row. A terminal run (or a selected attempt that is no longer live)
  // matches nothing here and stays read-only.
  const liveMatch = useMemo<AttemptCapabilities | null>(() => {
    if (selectedRow === null) {
      return null;
    }
    return (
      liveAttempts.find(
        (attempt) => attemptRowKey(attempt.activityId, attempt.attempt) === selectedRow.key
      ) ?? null
    );
  }, [liveAttempts, selectedRow]);

  // ONE shared intervene pipeline for both slots (header actions + composer);
  // `null` when the selected row is not currently live — read-only then.
  const intervention = useInterventionController({ namespace, workflowId, attempt: liveMatch });

  if (namespace === null) {
    return (
      <EmptyState
        description="Select a namespace to enumerate this workflow's agent attempts."
        title="No namespace selected"
      />
    );
  }

  return (
    <section className="flex flex-col gap-3">
      <header className="flex items-center justify-between gap-2">
        <h2 className="font-semibold text-foreground text-sm">Agent attempts</h2>
        <div className="flex flex-wrap items-center justify-end gap-2">
          {intervention === null ? null : <InterventionActions controller={intervention} />}
          <Button className="h-7 px-3 text-xs" onClick={refresh} type="button" variant="outline">
            Refresh live
          </Button>
        </div>
      </header>
      <AttemptRowList onSelect={setPickedKey} rows={rows} selectedKey={selectedRow?.key ?? null} />
      <TranscriptPanel target={target} />
      {intervention === null ? null : <InterventionComposer controller={intervention} />}
    </section>
  );
}

/**
 * Resolve the effective row: a manual pick wins while its row still exists;
 * otherwise a single row auto-selects (nothing else to choose). Several rows and
 * no pick selects nothing. Keyed on the durable identity, never an index.
 */
function resolveSelectedRow(
  rows: readonly AttemptNavigatorRow[],
  pickedKey: string | null
): AttemptNavigatorRow | null {
  const manual = pickedKey === null ? null : (rows.find((row) => row.key === pickedKey) ?? null);
  if (manual !== null) {
    return manual;
  }
  return rows.length === 1 ? (rows[0] ?? null) : null;
}

type AttemptRowListProps = {
  rows: readonly AttemptNavigatorRow[];
  selectedKey: string | null;
  onSelect: (key: string) => void;
};

function AttemptRowList({ rows, selectedKey, onSelect }: AttemptRowListProps) {
  if (rows.length === 0) {
    return (
      <EmptyState
        description="No agent attempt has run for this workflow yet."
        title="No attempts"
      />
    );
  }

  return (
    <ul className="flex flex-wrap gap-2" data-testid="attempt-list">
      {rows.map((row) => (
        <li key={row.key}>
          <Button
            aria-pressed={selectedKey === row.key}
            className={cn(
              'h-7 gap-1.5 px-3 text-xs',
              selectedKey === row.key && 'bg-surface-hover'
            )}
            onClick={() => onSelect(row.key)}
            type="button"
            variant="ghost"
          >
            <StatusDot status={ATTEMPT_KIT_STATUS[row.status]} />
            {row.isLegacy
              ? `activity ${row.activityId} · legacy attempt`
              : `activity ${row.activityId} · attempt ${row.attempt}`}
          </Button>
        </li>
      ))}
    </ul>
  );
}

const ATTEMPT_KIT_STATUS: Record<AttemptStatus, KitStatus> = {
  started: 'running',
  completed: 'healthy',
  failed: 'failed',
  cancelled: 'special',
};
