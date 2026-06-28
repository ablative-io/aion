import { EventIcon, type EventIconTone } from '@/components/EventIcon';
import { Badge } from '@/components/ui';
import { cn } from '@/lib/utils';

import type { TimelineEntry as TimelineEntryModel } from '../types';
import { ActivityGroup } from './ActivityGroup';
import { PayloadView } from './PayloadView';

type TimelineEntryProps = {
  entry: TimelineEntryModel;
  selected?: boolean;
  onSelect?: (entry: TimelineEntryModel) => void;
};

function TimelineEntry({ entry, selected = false, onSelect }: TimelineEntryProps) {
  const selectable = onSelect !== undefined;

  return (
    <li className="relative flex gap-4 pb-6 last:pb-0">
      <div className="absolute top-11 bottom-0 left-5 w-px bg-[var(--border-default)] last:hidden" />
      <div className="relative z-10">
        <EventIcon kind={entry.kind} tone={toneForEntry(entry)} />
      </div>
      <article
        aria-current={selected ? 'true' : undefined}
        className={cn(
          'min-w-0 flex-1 rounded-xl border bg-[var(--surface-elevated)] p-4',
          selected
            ? 'border-[var(--accent-cyan)] ring-1 ring-[var(--accent-cyan)]'
            : 'border-[var(--border-default)]'
        )}
      >
        <header className="flex flex-wrap items-start justify-between gap-3">
          <button
            className={cn(
              'min-w-0 space-y-1 text-left',
              selectable &&
                'cursor-pointer rounded-md focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--accent-cyan)]'
            )}
            disabled={!selectable}
            onClick={selectable ? () => onSelect(entry) : undefined}
            type="button"
          >
            <div className="flex flex-wrap items-center gap-2">
              <Badge variant="outline">seq {entry.sequence}</Badge>
              <time className="text-[var(--text-muted)] text-xs" dateTime={entry.recordedAt}>
                {formatRecordedAt(entry.recordedAt)}
              </time>
            </div>
            <h3 className="font-medium text-[var(--text-primary)]">{entry.summary}</h3>
          </button>
          <Badge variant="secondary">{kindLabel(entry)}</Badge>
        </header>
        <TimelineBody entry={entry} />
      </article>
    </li>
  );
}

function TimelineBody({ entry }: { entry: TimelineEntryModel }) {
  if (entry.kind === 'activity') {
    return (
      <>
        <ActivityGroup entry={entry} />
        {entry.payload === undefined ? null : <PayloadView payload={entry.payload} />}
      </>
    );
  }

  if (entry.kind === 'child') {
    return (
      <div className="mt-3 space-y-3">
        <a
          className="text-[var(--accent-cyan)] text-sm underline-offset-4 hover:underline"
          href={`/workflows/${entry.childWorkflowId}`}
        >
          Open child workflow {entry.childWorkflowId}
        </a>
        {entry.payload === undefined ? null : <PayloadView payload={entry.payload} />}
      </div>
    );
  }

  if (entry.kind === 'signal' && entry.direction === 'sent' && entry.targetWorkflowId) {
    return (
      <div className="mt-3 space-y-3">
        <a
          className="text-[var(--accent-cyan)] text-sm underline-offset-4 hover:underline"
          href={`/workflows/${entry.targetWorkflowId}`}
        >
          Open target workflow {entry.targetWorkflowId}
        </a>
        {entry.payload === undefined ? null : <PayloadView payload={entry.payload} />}
      </div>
    );
  }

  return entry.payload === undefined ? null : <PayloadView payload={entry.payload} />;
}

/** A precise lane label: known generic variants surface their sub-kind family. */
function kindLabel(entry: TimelineEntryModel): string {
  if (entry.kind === 'generic') {
    return entry.family ?? 'event';
  }

  return entry.kind;
}

type LifecycleEntry = Extract<TimelineEntryModel, { kind: 'lifecycle' }>;
type TimerEntry = Extract<TimelineEntryModel, { kind: 'timer' }>;
type ActivityEntry = Extract<TimelineEntryModel, { kind: 'activity' }>;

const LIFECYCLE_TONES: Record<LifecycleEntry['outcome'], EventIconTone> = {
  started: 'neutral',
  completed: 'success',
  failed: 'danger',
  cancelled: 'warning',
  'timed-out': 'warning',
};

const TIMER_TONES: Record<TimerEntry['status'], EventIconTone> = {
  started: 'info',
  fired: 'info',
  cancelled: 'info',
  completed: 'success',
  'timed-out': 'warning',
};

function toneForEntry(entry: TimelineEntryModel): EventIconTone {
  switch (entry.kind) {
    case 'lifecycle':
      return LIFECYCLE_TONES[entry.outcome];
    case 'timer':
      return TIMER_TONES[entry.status];
    case 'activity':
      return activityTone(entry.status);
    case 'child':
      return entry.status === 'failed' ? 'danger' : 'neutral';
    default:
      return 'neutral';
  }
}

function activityTone(status: ActivityEntry['status']): EventIconTone {
  if (status === 'completed') {
    return 'success';
  }

  return status === 'failed' ? 'danger' : 'neutral';
}

function formatRecordedAt(value: string): string {
  const date = new Date(value);

  if (Number.isNaN(date.getTime())) {
    return value;
  }

  return date.toLocaleString();
}

export { formatRecordedAt, TimelineEntry };
