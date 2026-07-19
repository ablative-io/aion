import { useEffect, useRef } from 'react';

import { isSelectedNamespace, requireSelectedNamespace, useNamespace } from '@/features/namespace';
import type { AionEventWebSocketManager } from '@/lib/api';
import type {
  AionEventHandler,
  AionEventSubscriptionFilter,
  ResyncHandler,
} from '@/lib/api/websocket';
import { configuredEventSocket } from '@/lib/config';
import type { Event, Namespace } from '@/types';

/**
 * The slice of the WebSocket manager an event subscription depends on.
 *
 * Consumers (and tests) inject a minimal stand-in that only implements
 * `subscribe`, so the dependency is narrowed to exactly that surface.
 */
export type EventSubscriptionManager = Pick<AionEventWebSocketManager, 'subscribe'>;

/**
 * Per-subscription state surfaced to callers. Kept intentionally small; the
 * subscription itself is fire-and-forget (the effect owns its lifetime).
 */
export type EventSubscriptionState = {
  enabled: boolean;
  namespace: Namespace | null;
};

type NamespaceScopedSubscriptionInput<TFilter extends AionEventSubscriptionFilter> = {
  /**
   * When false (or when the filter is null), no subscription is opened. This is
   * how callers gate subscriptions on terminal workflows or an unselected
   * namespace without reaching for a non-null assertion.
   */
  enabled?: boolean | undefined;
  afterSeq?: number | undefined;
  lastSeenSequence?: number | undefined;
  filter: Omit<TFilter, 'namespace'> | null;
  manager?: EventSubscriptionManager | undefined;
  onEvent: (event: Event) => void;
  onResync?: ResyncHandler | undefined;
};

export function namespaceSubscriptionFilter<TFilter extends AionEventSubscriptionFilter>(
  namespace: Namespace,
  filter: Omit<TFilter, 'namespace'>
): TFilter {
  const selectedNamespace = requireSelectedNamespace(namespace, 'subscribing to events');

  return { ...filter, namespace: selectedNamespace } as TFilter;
}

export function subscribeToNamespaceFilter<TFilter extends AionEventSubscriptionFilter>(
  manager: EventSubscriptionManager,
  namespace: Namespace,
  filter: Omit<TFilter, 'namespace'>,
  onEvent: (event: Event) => void,
  options: {
    afterSeq?: number | undefined;
    onResync?: ResyncHandler | undefined;
  } = {}
) {
  const handler: AionEventHandler = (event) => onEvent(event);

  return manager.subscribe(namespaceSubscriptionFilter<TFilter>(namespace, filter), handler, {
    lastSeenSequence: options.afterSeq,
    onResync: options.onResync,
  });
}

type ResyncHandlerRef = {
  current: ResyncHandler | undefined;
};

/**
 * Preserves an absent optional callback instead of turning it into a successful
 * no-op. The returned wrapper keeps React closures fresh, but fails explicitly
 * if a callback that existed when the subscription opened later disappears.
 */
export function resyncHandlerFromRef(ref: ResyncHandlerRef): ResyncHandler | undefined {
  if (ref.current === undefined) {
    return undefined;
  }

  return (context) => {
    const onResync = ref.current;
    if (onResync === undefined) {
      throw new Error('The live-state refetch callback became unavailable during recovery');
    }
    return onResync(context);
  };
}

export function useEventSubscription<TFilter extends AionEventSubscriptionFilter>({
  enabled = true,
  afterSeq,
  lastSeenSequence,
  filter,
  manager = configuredEventSocket,
  onEvent,
  onResync,
}: NamespaceScopedSubscriptionInput<TFilter>): EventSubscriptionState {
  const { selectedNamespace } = useNamespace();
  const active = enabled && filter !== null && isSelectedNamespace(selectedNamespace);

  // `lastSeenSequence` advances on every event, and `onEvent`/`onResync` are
  // fresh closures on every render. If any were in the effect's dependency
  // array, the socket would tear down and re-subscribe on every event. Refs keep
  // identity changes (`enabled` / `filter` / `manager` / namespace) as the only
  // reopen triggers. The manager uses the sequence as a durable cursor only for
  // per-workflow subscriptions; filtered/firehose reconnect live-only and pass
  // it back solely as context for the caller's full history refetch.
  const onEventRef = useRef(onEvent);
  onEventRef.current = onEvent;
  const onResyncRef = useRef(onResync);
  onResyncRef.current = onResync;
  const cursorRef = useRef<number | undefined>(undefined);
  cursorRef.current = lastSeenSequence ?? afterSeq;

  useEffect(() => {
    if (!(enabled && filter !== null) || !isSelectedNamespace(selectedNamespace)) {
      return;
    }

    return subscribeToNamespaceFilter<TFilter>(
      manager,
      selectedNamespace,
      filter,
      (event) => onEventRef.current(event),
      {
        afterSeq: cursorRef.current,
        onResync: resyncHandlerFromRef(onResyncRef),
      }
    );
  }, [enabled, filter, manager, selectedNamespace]);

  return {
    enabled: active,
    namespace: isSelectedNamespace(selectedNamespace) ? selectedNamespace : null,
  };
}
