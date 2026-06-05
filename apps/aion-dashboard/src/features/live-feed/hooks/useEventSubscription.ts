import { useEffect, useMemo, useRef, useState } from 'react';
import type {
  AionEventContext,
  AionEventHandler,
  AionEventSubscriptionFilter,
  ResyncContext,
} from '@/lib/api';
import { type AionEventWebSocketManager, aionEventSocket } from '@/lib/api';
import type { Event } from '@/types';

export type EventSubscriptionManager = Pick<AionEventWebSocketManager, 'subscribe'>;

export type EventSubscriptionInput = {
  enabled?: boolean;
  filter: AionEventSubscriptionFilter | null;
  lastSeenSequence?: number;
  manager?: EventSubscriptionManager;
  onEvent: AionEventHandler;
  onResync?: (context: ResyncContext) => void;
};

export type EventSubscriptionState = {
  resyncContext: ResyncContext | null;
  resyncCount: number;
};

export function useEventSubscription({
  enabled = true,
  filter,
  lastSeenSequence,
  manager = aionEventSocket,
  onEvent,
  onResync,
}: EventSubscriptionInput): EventSubscriptionState {
  const eventHandlerRef = useRef(onEvent);
  const lastSeenSequenceRef = useRef(lastSeenSequence);
  const resyncHandlerRef = useRef(onResync);
  const [resyncState, setResyncState] = useState<EventSubscriptionState>({
    resyncContext: null,
    resyncCount: 0,
  });

  eventHandlerRef.current = onEvent;
  lastSeenSequenceRef.current = lastSeenSequence;
  resyncHandlerRef.current = onResync;

  useEffect(() => {
    if (!enabled || filter === null) {
      return;
    }

    return manager.subscribe(
      filter,
      (event: Event, context: AionEventContext) => {
        eventHandlerRef.current(event, context);
      },
      {
        lastSeenSequence: lastSeenSequenceRef.current,
        onResync: (context) => {
          setResyncState((current) => ({
            resyncContext: {
              ...context,
              lastSeenSequence: lastSeenSequenceRef.current ?? context.lastSeenSequence,
            },
            resyncCount: current.resyncCount + 1,
          }));
          resyncHandlerRef.current?.({
            ...context,
            lastSeenSequence: lastSeenSequenceRef.current ?? context.lastSeenSequence,
          });
        },
      }
    );
  }, [enabled, filter, manager]);

  return useMemo(() => resyncState, [resyncState]);
}
