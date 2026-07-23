import { type CSSProperties, memo, type RefCallback } from 'react';
import { EventIcon, type EventIconTone } from '@/components/EventIcon';
import { Badge } from '@/components/ui';
import { cn } from '@/lib/utils';

import { eventContainingPayload } from '../lib/timeline';
import type { TimelineEntry as TimelineEntryModel } from '../types';
import { ActivityGroup } from './ActivityGroup';
import { PayloadView } from './PayloadView';

type TimelineEntryProps = {
  entry: TimelineEntryModel;
  selected?: boolean | undefined;
  onSelect?: ((entry: TimelineEntryModel) => void) | undefined;
  virtual?: {
    index: number;
    start: number;
    measureElement: RefCallback<HTMLLIElement>;
  };
};

const TimelineEntry = memo(function TimelineEntry({
  entry,
  selected = false,
  onSelect,
  virtual,
}: TimelineEntryProps) {
  const selectable = onSelect !== undefined;
  const virtualStyle: CSSProperties | undefined =
    virtual === undefined
      ? undefined
      : {
          position: 'absolute',
          top: 0,
          left: 0,
          width: '100%',
          transform: `translateY(${virtual.start}px)`,
        };

  return (
    <li
      className="relative flex gap-4 pb-6 last:pb-0"
      data-index={virtual?.index}
      ref={virtual?.measureElement}
      style={virtualStyle}
    >
      <div className="absolute top-11 bottom-0 left-5 w-px bg-border last:hidden" />
      <div className="relative z-10">
        <EventIcon kind={entry.kind} tone={toneForEntry(entry)} />
      </div>
      <article
        aria-current={selected ? 'true' : undefined}
        className={cn(
          'min-w-0 flex-1 rounded-xl border bg-surface-elevated p-4',
          selected ? 'border-primary ring-1 ring-primary' : 'border-border'
        )}
      >
        <header className="flex flex-wrap items-start justify-between gap-3">
          <button
            className={cn(
              'min-w-0 space-y-1 text-left',
              selectable &&
                'cursor-pointer rounded-md focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring'
            )}
            disabled={!selectable}
            onClick={selectable ? () => onSelect(entry) : undefined}
            type="button"
          >
            <div className="flex flex-wrap items-center gap-2">
              <Badge variant="outline">seq {entry.sequence}</Badge>
              <time className="text-muted-foreground text-xs" dateTime={entry.recordedAt}>
                {formatRecordedAt(entry.recordedAt)}
              </time>
            </div>
            <h3 className="font-medium text-foreground">{entry.summary}</h3>
          </button>
          <Badge variant="secondary">{kindLabel(entry)}</Badge>
        </header>
        <TimelineBody entry={entry} />
      </article>
    </li>
  );
});

function TimelineBody({ entry }: { entry: TimelineEntryModel }) {
  if (entry.kind === 'activity') {
    return (
      <>
        <ActivityGroup entry={entry} />
        {entry.payload === undefined ? null : (
          <PayloadView
            event={eventContainingPayload(entry.events, entry.payload)}
            payload={entry.payload}
          />
        )}
      </>
    );
  }

  if (entry.kind === 'child') {
    return (
      <div className="mt-3 space-y-3">
        <a
          className="text-primary text-sm underline-offset-4 hover:underline"
          href={`/workflows/${entry.childWorkflowId}`}
        >
          Open child workflow {entry.childWorkflowId}
        </a>
        {entry.payload === undefined ? null : (
          <PayloadView
            event={eventContainingPayload(entry.events, entry.payload)}
            payload={entry.payload}
          />
        )}
      </div>
    );
  }

  if (entry.kind === 'signal' && entry.direction === 'sent' && entry.targetWorkflowId) {
    return (
      <div className="mt-3 space-y-3">
        <a
          className="text-primary text-sm underline-offset-4 hover:underline"
          href={`/workflows/${entry.targetWorkflowId}`}
        >
          Open target workflow {entry.targetWorkflowId}
        </a>
        {entry.payload === undefined ? null : (
          <PayloadView
            event={eventContainingPayload(entry.events, entry.payload)}
            payload={entry.payload}
          />
        )}
      </div>
    );
  }

  return entry.payload === undefined ? null : (
    <PayloadView
      event={eventContainingPayload(entry.events, entry.payload)}
      payload={entry.payload}
    />
  );
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
