import { useMemo, useState } from 'react';

import { EmptyState } from '@/components/EmptyState';
import { isSelectedNamespace, useNamespace } from '@/features/namespace';
import {
  eventRecordedAt,
  eventSequence,
  mergeEventsBySequence,
} from '@/features/workflow-detail/lib/timeline';
import type { FirehoseEventSubscriptionFilter } from '@/lib/api';
import type { Event } from '@/types';
import { useConnectionStatus } from '../hooks/useConnectionStatus';
import { type EventSubscriptionManager, useEventSubscription } from '../hooks/useEventSubscription';
import { ConnectionIndicatorContent } from './ConnectionIndicator';

export type FirehoseFeedProps = {
  manager?: EventSubscriptionManager;
  maxEvents?: number;
};

export function FirehoseFeed({ manager, maxEvents = 100 }: FirehoseFeedProps) {
  const { selectedNamespace } = useNamespace();
  const status = useConnectionStatus();
  const [events, setEvents] = useState<Event[]>([]);
  const subscriptionFilter = useMemo<FirehoseEventSubscriptionFilter | null>(() => {
    if (!isSelectedNamespace(selectedNamespace)) {
      return null;
    }

    return { kind: 'firehose', namespace: selectedNamespace };
  }, [selectedNamespace]);

  useEventSubscription({
    enabled: subscriptionFilter !== null,
    filter: subscriptionFilter,
    manager,
    onEvent: (event) => {
      setEvents((current) => mergeEventsBySequence(current, [event]).slice(-maxEvents));
    },
  });

  return <FirehoseFeedContent events={events} namespace={selectedNamespace} status={status} />;
}

export type FirehoseFeedContentProps = {
  events: readonly Event[];
  namespace: string | null;
  status: ReturnType<typeof useConnectionStatus>;
};

export function FirehoseFeedContent({ events, namespace, status }: FirehoseFeedContentProps) {
  if (!isSelectedNamespace(namespace)) {
    return (
      <EmptyState
        description="Select a namespace before opening the live firehose."
        title="No namespace selected"
      />
    );
  }

  return (
    <section className="space-y-4" aria-label="Live event firehose">
      <header className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h2 className="font-semibold text-lg text-[var(--text-primary)]">Live firehose</h2>
          <p className="text-[var(--text-muted)] text-sm">Namespace {namespace}</p>
        </div>
        <ConnectionIndicatorContent status={status} />
      </header>

      {status !== 'connected' ? (
        <div className="rounded-lg border border-amber-500/30 bg-amber-500/10 p-3 text-amber-800 text-sm">
          {status === 'reconnecting'
            ? 'Socket dropped; reconnecting and resubscribing to the firehose.'
            : 'Live socket is disconnected.'}
        </div>
      ) : null}

      {events.length === 0 ? (
        <EmptyState
          description="Events will appear here as soon as the server streams them."
          title="No live events yet"
        />
      ) : (
        <ol className="space-y-2" aria-label="Firehose events">
          {events.toReversed().map((event) => (
            <li
              className="rounded-lg border border-[var(--border-muted)] bg-[var(--surface-panel)] p-3"
              key={eventSequence(event)}
            >
              <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm">
                <span className="font-mono text-[var(--text-muted)]">
                  seq {eventSequence(event)}
                </span>
                <span className="font-medium text-[var(--text-primary)]">{event.type}</span>
                <span className="text-[var(--text-muted)]">
                  workflow {event.data.envelope.workflow_id}
                </span>
              </div>
              <time className="text-[var(--text-muted)] text-xs" dateTime={eventRecordedAt(event)}>
                {eventRecordedAt(event)}
              </time>
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}
