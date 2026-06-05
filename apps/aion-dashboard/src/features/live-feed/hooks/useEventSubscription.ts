import { useEffect } from 'react';

import { isSelectedNamespace, requireSelectedNamespace, useNamespace } from '@/features/namespace';
import { aionWebSocketManager, type EventSubscriptionManager } from '@/lib/api/websocket';
import type { Event, Namespace } from '@/types';

type NamespaceScopedSubscriptionInput<TFilter extends object> = {
  afterSeq?: number;
  filter: TFilter;
  onEvent: (event: Event) => void;
  manager?: EventSubscriptionManager;
};

export function namespaceSubscriptionFilter<TFilter extends object>(
  namespace: Namespace,
  filter: TFilter,
  afterSeq?: number
): TFilter & { namespace: Namespace; afterSeq?: number } {
  const selectedNamespace = requireSelectedNamespace(namespace, 'subscribing to events');

  return afterSeq === undefined
    ? { ...filter, namespace: selectedNamespace }
    : { ...filter, afterSeq, namespace: selectedNamespace };
}

export function subscribeToNamespaceFilter<TFilter extends object>(
  manager: EventSubscriptionManager,
  namespace: Namespace,
  filter: TFilter,
  onEvent: (event: Event) => void,
  afterSeq?: number
) {
  return manager.subscribe(namespaceSubscriptionFilter(namespace, filter, afterSeq), onEvent);
}

export function useEventSubscription<TFilter extends object>({
  afterSeq,
  filter,
  manager = aionWebSocketManager,
  onEvent,
}: NamespaceScopedSubscriptionInput<TFilter>) {
  const { selectedNamespace } = useNamespace();

  useEffect(() => {
    if (!isSelectedNamespace(selectedNamespace)) {
      return;
    }

    return subscribeToNamespaceFilter(manager, selectedNamespace, filter, onEvent, afterSeq);
  }, [afterSeq, filter, manager, onEvent, selectedNamespace]);
}
