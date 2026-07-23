import { ChevronRight } from 'lucide-react';
import { useState } from 'react';
import { Badge, Button } from '@/components/ui';
import { cn } from '@/lib/utils';

import type { ActivityTimelineEntry } from '../types';
import { PayloadView } from './PayloadView';

type ActivityGroupProps = {
  entry: ActivityTimelineEntry;
};

function ActivityGroup({ entry }: ActivityGroupProps) {
  const [open, setOpen] = useState(false);

  return (
    <div>
      <div className="flex flex-wrap items-center gap-2">
        <Badge variant="secondary">{entry.status}</Badge>
        <span className="text-muted-foreground text-sm">Activity id {entry.activityId}</span>
        {entry.failures.length > 0 ? (
          <span className="text-warning text-sm">{entry.failures.length} failed attempt(s)</span>
        ) : null}
      </div>
      <Button
        aria-expanded={open}
        className="mt-3 h-auto px-0 text-secondary-foreground"
        onClick={() => setOpen((current) => !current)}
        type="button"
        variant="ghost"
      >
        <ChevronRight className={cn('size-4 transition-transform', open && 'rotate-90')} />
        Activity lifecycle details
      </Button>
      {open ? (
        <div className="mt-3 space-y-3 rounded-lg border border-border p-3">
          <LifecycleLine label="Scheduled" present={entry.scheduled !== null} />
          <LifecycleLine label="Started" present={entry.started !== null} />
          {entry.failures.map((failure) => (
            <div className="rounded-md bg-danger-glow p-3" key={failure.sequence}>
              <div className="font-medium text-danger text-sm">
                Failed attempt {failure.attempt} at sequence {failure.sequence}
              </div>
              <PayloadView event={failure.event} label="Failure payload" payload={failure.error} />
            </div>
          ))}
          <LifecycleLine label="Completed" present={entry.completed !== null} />
          <LifecycleLine label="Cancelled" present={entry.cancelled !== null} />
        </div>
      ) : null}
    </div>
  );
}

function LifecycleLine({ label, present }: { label: string; present: boolean }) {
  return (
    <div className="flex items-center justify-between text-sm">
      <span className="text-secondary-foreground">{label}</span>
      <span className={present ? 'text-success' : 'text-muted-foreground'}>
        {present ? 'seen' : 'not seen'}
      </span>
    </div>
  );
}

export { ActivityGroup };
