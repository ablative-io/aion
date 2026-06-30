import type { Event } from '@/types';

import { projectTimeline } from '../lib/timeline';
import type { TimelineEntry as TimelineEntryModel } from '../types';
import { TimelineEntry } from './TimelineEntry';

type EventTimelineProps = {
  events?: readonly Event[] | undefined;
  entries?: readonly TimelineEntryModel[] | undefined;
  selectedSequence?: number | null | undefined;
  onSelect?: ((entry: TimelineEntryModel) => void) | undefined;
};

function EventTimeline({ events, entries, selectedSequence = null, onSelect }: EventTimelineProps) {
  const timelineEntries = (entries ?? projectTimeline(events ?? [])).toSorted(
    (left, right) => left.sequence - right.sequence
  );

  return (
    <ol aria-label="Workflow event timeline" className="space-y-0">
      {timelineEntries.map((entry) => (
        <TimelineEntry
          entry={entry}
          key={entry.id}
          onSelect={onSelect}
          selected={selectedSequence === entry.sequence}
        />
      ))}
    </ol>
  );
}

export { EventTimeline };
