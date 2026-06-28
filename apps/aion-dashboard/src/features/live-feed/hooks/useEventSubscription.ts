import { useEffect } from 'react';

import { isSelectedNamespace, requireSelectedNamespace, useNamespace } from '@/features/namespace';
import { type AionEventWebSocketManager, aionEventSocket } from '@/lib/api';
import type {
  AionEventHandler,
  AionEventSubscriptionFilter,
  ResyncContext,
} from '@/lib/api/websocket';
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
  enabled?: boolean;
  afterSeq?: number;
  lastSeenSequence?: number;
  filter: Omit<TFilter, 'namespace'> | null;
  manager?: EventSubscriptionManager;
  onEvent: (event: Event) => void;
  onResync?: (context: ResyncContext) => void;
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
  options: { afterSeq?: number; onResync?: (context: ResyncContext) => void } = {}
) {
  const handler: AionEventHandler = (event) => onEvent(event);

  return manager.subscribe(namespaceSubscriptionFilter<TFilter>(namespace, filter), handler, {
    lastSeenSequence: options.afterSeq,
    onResync: options.onResync,
  });
}

export function useEventSubscription<TFilter extends AionEventSubscriptionFilter>({
  enabled = true,
  afterSeq,
  lastSeenSequence,
  filter,
  manager = aionEventSocket,
  onEvent,
  onResync,
}: NamespaceScopedSubscriptionInput<TFilter>): EventSubscriptionState {
  const { selectedNamespace } = useNamespace();
  const active = enabled && filter !== null && isSelectedNamespace(selectedNamespace);

  useEffect(() => {
    if (!(enabled && filter !== null) || !isSelectedNamespace(selectedNamespace)) {
      return;
    }

    return subscribeToNamespaceFilter<TFilter>(manager, selectedNamespace, filter, onEvent, {
      afterSeq: lastSeenSequence ?? afterSeq,
      onResync,
    });
  }, [enabled, afterSeq, lastSeenSequence, filter, manager, onEvent, onResync, selectedNamespace]);

  return {
    enabled: active,
    namespace: isSelectedNamespace(selectedNamespace) ? selectedNamespace : null,
  };
}
