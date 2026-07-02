import { X } from 'lucide-react';

import { Badge, Button } from '@/components/ui';
import { cn } from '@/lib/utils';
import type { Event, Namespace, WorkflowId } from '@/types';

import { computeReopen, type ReopenActivityRow, type ReopenDisposition } from './computeReopen';
import { useReopen } from './useReopen';

type ReopenDiffProps = {
  /** The (failed) run's recorded history. Read-only — the dashboard never writes. */
  history: readonly Event[];
  workflowId: WorkflowId;
  /** Auth scope for the reopen command; `null` disables the commit. */
  namespace: Namespace | null;
  onClose: () => void;
  /** Notified with the reopened run id after a committed reopen succeeds. */
  onReopened?: (runId: string) => void;
};

// Disposition palette: semantic status tokens (green = preserved, amber = will
// re-run, red = superseded), per the VISION doc's reading of the diff.
const DISPOSITION_COLOR: Record<ReopenDisposition, string> = {
  reused: 'var(--status-success)',
  redispatch: 'var(--status-warning)',
  superseded: 'var(--status-danger)',
};

const DISPOSITION_LABEL: Record<ReopenDisposition, string> = {
  reused: 'preserved',
  redispatch: 'will re-run',
  superseded: 'superseded',
};

/**
 * Before/after preview of a reopen, computed purely from recorded history
 * (VISION §4.3): green/preserved = reused recorded results, amber = will
 * re-dispatch, struck = superseded terminal failure.
 *
 * Committing a reopen (VISION §6.2) POSTs to `POST /workflows/reopen` and
 * surfaces the outcome HONESTLY: a success reports the reopened run + projected
 * status, and a rejection (non-reopenable terminal, absent workflow,
 * authorization) is shown as a visible error — never swallowed and never
 * fabricated. The live event stream drives the workflow back to Running.
 */
export function ReopenDiff({
  history,
  workflowId,
  namespace,
  onClose,
  onReopened,
}: ReopenDiffProps) {
  const plan = computeReopen(history);
  const beforeRows = plan.rows.filter((row) => row.disposition !== 'redispatch');
  const afterRows = plan.rows.filter((row) => row.disposition !== 'superseded');
  const { submit, submitState, lastResult, error } = useReopen({ namespace });

  const commit = (): void => {
    void submit(workflowId)
      .then((result) => {
        if (result !== null) {
          onReopened?.(result.runId);
        }
      })
      .catch(() => {
        // The error is already captured as visible state by the hook; swallow the
        // rejection here only to avoid an unhandled-promise warning.
      });
  };

  return (
    <aside
      aria-label={`Reopen preview for workflow ${workflowId}`}
      className="flex h-full w-full max-w-2xl flex-col gap-4 border-border border-l bg-card p-5"
      data-testid="reopen-diff"
    >
      <header className="flex items-start justify-between gap-4">
        <div>
          <p className="text-muted-foreground text-xs uppercase tracking-wide">Reopen preview</p>
          <h2 className="font-semibold text-foreground text-lg">Workflow {workflowId}</h2>
        </div>
        <Button aria-label="Close reopen preview" onClick={onClose} type="button" variant="ghost">
          <X className="size-4" />
        </Button>
      </header>

      {plan.reopenable ? (
        <p className="text-secondary-foreground text-sm">
          Reopening re-dispatches {plan.reopened.length} terminal-failed activit
          {plan.reopened.length === 1 ? 'y' : 'ies'}; the reset cursor supersedes the recorded
          failure from sequence {plan.cursorResetSeq}.
        </p>
      ) : (
        <p className="text-muted-foreground text-sm">
          This run has no terminal-failed activity without a later success. Nothing would
          re-dispatch — reopen is not applicable.
        </p>
      )}

      <div className="grid flex-1 grid-cols-2 gap-4 overflow-auto">
        <DiffColumn rows={beforeRows} title="Before (recorded)" />
        <DiffColumn rows={afterRows} title="After (projected)" />
      </div>

      <footer className="flex flex-col gap-2 border-border border-t pt-4">
        <Button
          data-testid="reopen-commit"
          disabled={!plan.reopenable || namespace === null || submitState === 'submitting'}
          onClick={commit}
          type="button"
          variant="default"
        >
          {submitState === 'submitting' ? 'Reopening…' : 'Commit reopen'}
        </Button>
        <ReopenNotice
          error={error}
          namespace={namespace}
          reopenable={plan.reopenable}
          result={lastResult}
          submitState={submitState}
        />
      </footer>
    </aside>
  );
}

type ReopenNoticeProps = {
  submitState: ReturnType<typeof useReopen>['submitState'];
  result: ReturnType<typeof useReopen>['lastResult'];
  error: Error | null;
  reopenable: boolean;
  namespace: Namespace | null;
};

/** Surface the reopen outcome / gating reason as distinct, honest visible state. */
function ReopenNotice({ submitState, result, error, reopenable, namespace }: ReopenNoticeProps) {
  if (submitState === 'error' && error !== null) {
    return (
      <p className="text-destructive text-xs" data-testid="reopen-error" role="alert">
        {error.message}
      </p>
    );
  }
  if (submitState === 'settled' && result !== null) {
    return (
      <Badge
        className="self-start border-success/40 bg-success-glow text-success"
        data-testid="reopen-outcome"
        variant="outline"
      >
        Reopened as run {result.runId} ({result.status})
      </Badge>
    );
  }
  if (!reopenable) {
    return (
      <p className="text-muted-foreground text-xs" role="note">
        Nothing would re-dispatch — this run has no terminal-failed activity to reopen.
      </p>
    );
  }
  if (namespace === null) {
    return (
      <p className="text-muted-foreground text-xs" role="note">
        Select a namespace to scope the reopen command.
      </p>
    );
  }
  return (
    <p className="text-muted-foreground text-xs" role="note">
      Committing re-dispatches the failed tail; the live view reflects the run returning to Running.
    </p>
  );
}

type DiffColumnProps = {
  title: string;
  rows: ReopenActivityRow[];
};

function DiffColumn({ title, rows }: DiffColumnProps) {
  return (
    <div className="flex flex-col gap-2">
      <h3 className="font-medium text-secondary-foreground text-xs uppercase tracking-wide">
        {title}
      </h3>
      {rows.length === 0 ? (
        <p className="text-muted-foreground text-sm">No activities.</p>
      ) : (
        <ul className="flex flex-col gap-1.5">
          {rows.map((row) => (
            <DiffRow key={`${title}:${row.disposition}:${row.activityId}`} row={row} />
          ))}
        </ul>
      )}
    </div>
  );
}

function DiffRow({ row }: { row: ReopenActivityRow }) {
  const struck = row.disposition === 'superseded';

  return (
    <li
      className="flex flex-col gap-1 rounded-md border border-border bg-surface-base p-2.5"
      data-disposition={row.disposition}
    >
      <div className="flex items-center gap-2">
        <span
          aria-hidden="true"
          className="size-2 rounded-full"
          style={{ backgroundColor: DISPOSITION_COLOR[row.disposition] }}
        />
        <span
          className={cn(
            'font-mono text-foreground text-sm',
            struck && 'text-muted-foreground line-through'
          )}
        >
          {row.activityType ?? `activity ${row.activityId}`}
        </span>
        <span className="ml-auto text-muted-foreground text-xs">
          {DISPOSITION_LABEL[row.disposition]}
        </span>
      </div>
      <div className="flex items-center gap-3 text-muted-foreground text-xs">
        <span>seq {row.decidedAtSeq}</span>
        {row.attempts > 0 ? <span>{row.attempts} attempt(s)</span> : null}
      </div>
      {row.failureMessage !== null && row.disposition === 'superseded' ? (
        <p className="text-destructive text-xs">{row.failureMessage}</p>
      ) : null}
    </li>
  );
}
