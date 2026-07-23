import { useVirtualizer } from '@tanstack/react-virtual';
import { useEffect, useMemo, useRef } from 'react';

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
  const timelineEntries = useMemo(
    () =>
      (entries ?? projectTimeline(events ?? [])).toSorted(
        (left, right) => left.sequence - right.sequence
      ),
    [entries, events]
  );
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const virtualizer = useVirtualizer({
    count: timelineEntries.length,
    estimateSize: () => 144,
    getItemKey: (index) => timelineEntries[index]?.id ?? index,
    getScrollElement: () => scrollRef.current,
    initialRect: { width: 1024, height: 640 },
    overscan: 4,
  });
  const selectedIndex = useMemo(
    () => timelineEntries.findIndex((entry) => entry.sequence === selectedSequence),
    [selectedSequence, timelineEntries]
  );

  useEffect(() => {
    if (selectedIndex >= 0) {
      virtualizer.scrollToIndex(selectedIndex, { align: 'auto' });
    }
  }, [selectedIndex, virtualizer]);

  return (
    <div className="max-h-[640px] overflow-y-auto overscroll-contain" ref={scrollRef}>
      <ol
        aria-label="Workflow event timeline"
        className="relative"
        style={{ height: virtualizer.getTotalSize() }}
      >
        {virtualizer.getVirtualItems().map((virtualRow) => {
          const entry = timelineEntries[virtualRow.index];
          return entry === undefined ? null : (
            <TimelineEntry
              entry={entry}
              key={entry.id}
              onSelect={onSelect}
              selected={selectedSequence === entry.sequence}
              virtual={{
                index: virtualRow.index,
                measureElement: virtualizer.measureElement,
                start: virtualRow.start,
              }}
            />
          );
        })}
      </ol>
    </div>
  );
}

export { EventTimeline };
