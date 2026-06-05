import { EventIcon, type EventIconTone } from '@/components/EventIcon';
import { Badge } from '@/components/ui';

import type { TimelineEntry as TimelineEntryModel } from '../types';
import { ActivityGroup } from './ActivityGroup';
import { PayloadView } from './PayloadView';

type TimelineEntryProps = {
  entry: TimelineEntryModel;
};

function TimelineEntry({ entry }: TimelineEntryProps) {
  return (
    <li className="relative flex gap-4 pb-6 last:pb-0">
      <div className="absolute top-11 bottom-0 left-5 w-px bg-[var(--border-default)] last:hidden" />
      <div className="relative z-10">
        <EventIcon kind={entry.kind} tone={toneForEntry(entry)} />
      </div>
      <article className="min-w-0 flex-1 rounded-xl border border-[var(--border-default)] bg-[var(--surface-elevated)] p-4">
        <header className="flex flex-wrap items-start justify-between gap-3">
          <div className="space-y-1">
            <div className="flex flex-wrap items-center gap-2">
              <Badge variant="outline">seq {entry.sequence}</Badge>
              <time className="text-[var(--text-muted)] text-xs" dateTime={entry.recordedAt}>
                {formatRecordedAt(entry.recordedAt)}
              </time>
            </div>
            <h3 className="font-medium text-[var(--text-primary)]">{entry.summary}</h3>
          </div>
          <Badge variant="secondary">{entry.kind}</Badge>
        </header>
        <TimelineBody entry={entry} />
      </article>
    </li>
  );
}

function TimelineBody({ entry }: TimelineEntryProps) {
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

  return entry.payload === undefined ? null : <PayloadView payload={entry.payload} />;
}

function toneForEntry(entry: TimelineEntryModel): EventIconTone {
  if (entry.kind === 'lifecycle') {
    if (entry.outcome === 'completed') {
      return 'success';
    }

    if (entry.outcome === 'failed') {
      return 'danger';
    }

    if (entry.outcome === 'cancelled' || entry.outcome === 'timed-out') {
      return 'warning';
    }
  }

  if (entry.kind === 'activity' && entry.status === 'completed') {
    return 'success';
  }

  if (entry.kind === 'activity' && entry.status === 'failed') {
    return 'danger';
  }

  if (entry.kind === 'child' && entry.status === 'failed') {
    return 'danger';
  }

  if (entry.kind === 'timer') {
    return 'info';
  }

  return 'neutral';
}

function formatRecordedAt(value: string): string {
  const date = new Date(value);

  if (Number.isNaN(date.getTime())) {
    return value;
  }

  return date.toLocaleString();
}

export { formatRecordedAt, TimelineEntry };
