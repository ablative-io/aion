import { X } from 'lucide-react';

import { Button } from '@/components/ui';
import { cn } from '@/lib/utils';
import type { Event } from '@/types';

import { computeReopen, type ReopenActivityRow, type ReopenDisposition } from './computeReopen';

type ReopenDiffProps = {
  /** The (failed) run's recorded history. Read-only — the dashboard never writes. */
  history: readonly Event[];
  workflowId: string;
  onClose: () => void;
};

// Disposition palette. Reused/superseded reuse house tokens; "will re-run" uses a
// literal amber because no amber token exists in index.css yet and S6 must not edit
// that shared file — a missing var() would render transparent (a silent failure).
const AMBER = '#f59e0b';

const DISPOSITION_COLOR: Record<ReopenDisposition, string> = {
  reused: 'var(--accent-cyan)',
  redispatch: AMBER,
  superseded: 'var(--destructive)',
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
 * No-silent-failure (VISION §6.2): the commit button is rendered but DISABLED
 * with an explicit "command API not yet available" line. Issuing a reopen is a
 * server command (invariant 3) and the command surface is not on the wire today
 * (PHASE1-REMAINING-PLAN §8). We never render a dead button with no explanation,
 * and we never fabricate a success.
 */
export function ReopenDiff({ history, workflowId, onClose }: ReopenDiffProps) {
  const plan = computeReopen(history);
  const beforeRows = plan.rows.filter((row) => row.disposition !== 'redispatch');
  const afterRows = plan.rows.filter((row) => row.disposition !== 'superseded');

  return (
    <aside
      aria-label={`Reopen preview for workflow ${workflowId}`}
      className="flex h-full w-full max-w-2xl flex-col gap-4 border-[var(--border-default)] border-l bg-[var(--card)] p-5"
      data-testid="reopen-diff"
    >
      <header className="flex items-start justify-between gap-4">
        <div>
          <p className="text-[var(--text-muted)] text-xs uppercase tracking-wide">Reopen preview</p>
          <h2 className="font-semibold text-[var(--text-primary)] text-lg">
            Workflow {workflowId}
          </h2>
        </div>
        <Button aria-label="Close reopen preview" onClick={onClose} type="button" variant="ghost">
          <X className="size-4" />
        </Button>
      </header>

      {plan.reopenable ? (
        <p className="text-[var(--text-secondary)] text-sm">
          Reopening re-dispatches {plan.reopened.length} terminal-failed activit
          {plan.reopened.length === 1 ? 'y' : 'ies'}; the reset cursor supersedes the recorded
          failure from sequence {plan.cursorResetSeq}.
        </p>
      ) : (
        <p className="text-[var(--text-muted)] text-sm">
          This run has no terminal-failed activity without a later success. Nothing would
          re-dispatch — reopen is not applicable.
        </p>
      )}

      <div className="grid flex-1 grid-cols-2 gap-4 overflow-auto">
        <DiffColumn rows={beforeRows} title="Before (recorded)" />
        <DiffColumn rows={afterRows} title="After (projected)" />
      </div>

      <footer className="flex flex-col gap-2 border-[var(--border-default)] border-t pt-4">
        <Button disabled type="button" variant="default">
          Commit reopen
        </Button>
        <p className="text-[var(--text-muted)] text-xs" role="note">
          Commit is disabled: the server command API for reopen is not yet available
          (PHASE1-REMAINING-PLAN §8). This is a preview only; the dashboard never writes a
          workflow's history.
        </p>
      </footer>
    </aside>
  );
}

type DiffColumnProps = {
  title: string;
  rows: ReopenActivityRow[];
};

function DiffColumn({ title, rows }: DiffColumnProps) {
  return (
    <div className="flex flex-col gap-2">
      <h3 className="font-medium text-[var(--text-secondary)] text-xs uppercase tracking-wide">
        {title}
      </h3>
      {rows.length === 0 ? (
        <p className="text-[var(--text-muted)] text-sm">No activities.</p>
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
      className="flex flex-col gap-1 rounded-md border border-[var(--border-default)] bg-[var(--surface-base)] p-2.5"
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
            'font-mono text-[var(--text-primary)] text-sm',
            struck && 'text-[var(--text-muted)] line-through'
          )}
        >
          {row.activityType ?? `activity ${row.activityId}`}
        </span>
        <span className="ml-auto text-[var(--text-muted)] text-xs">
          {DISPOSITION_LABEL[row.disposition]}
        </span>
      </div>
      <div className="flex items-center gap-3 text-[var(--text-muted)] text-xs">
        <span>seq {row.decidedAtSeq}</span>
        {row.attempts > 0 ? <span>{row.attempts} attempt(s)</span> : null}
      </div>
      {row.failureMessage !== null && row.disposition === 'superseded' ? (
        <p className="text-[var(--destructive)] text-xs">{row.failureMessage}</p>
      ) : null}
    </li>
  );
}
