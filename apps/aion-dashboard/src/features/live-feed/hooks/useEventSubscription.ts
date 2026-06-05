import { useEffect } from 'react';

import { isSelectedNamespace, requireSelectedNamespace, useNamespace } from '@/features/namespace';
import { aionEventSocket, type AionEventWebSocketManager } from '@/lib/api';
import type { AionEventHandler, AionEventSubscriptionFilter } from '@/lib/api/websocket';
import type { Event, Namespace } from '@/types';

type NamespaceScopedSubscriptionInput<TFilter extends AionEventSubscriptionFilter> = {
  afterSeq?: number;
  filter: Omit<TFilter, 'namespace'>;
  manager?: Pick<AionEventWebSocketManager, 'subscribe'>;
  onEvent: (event: Event) => void;
};

export function namespaceSubscriptionFilter<TFilter extends AionEventSubscriptionFilter>(
  namespace: Namespace,
  filter: Omit<TFilter, 'namespace'>
): TFilter {
  const selectedNamespace = requireSelectedNamespace(namespace, 'subscribing to events');

  return { ...filter, namespace: selectedNamespace } as TFilter;
}

export function subscribeToNamespaceFilter<TFilter extends AionEventSubscriptionFilter>(
  manager: Pick<AionEventWebSocketManager, 'subscribe'>,
  namespace: Namespace,
  filter: Omit<TFilter, 'namespace'>,
  onEvent: (event: Event) => void,
  afterSeq?: number
) {
  const handler: AionEventHandler = (event) => onEvent(event);

  return manager.subscribe(namespaceSubscriptionFilter<TFilter>(namespace, filter), handler, {
    lastSeenSequence: afterSeq,
  });
}

export function useEventSubscription<TFilter extends AionEventSubscriptionFilter>({
  afterSeq,
  filter,
  manager = aionEventSocket,
  onEvent,
}: NamespaceScopedSubscriptionInput<TFilter>) {
  const { selectedNamespace } = useNamespace();

  useEffect(() => {
    if (!isSelectedNamespace(selectedNamespace)) {
      return;
    }

    return subscribeToNamespaceFilter<TFilter>(
      manager,
      selectedNamespace,
      filter,
      onEvent,
      afterSeq
    );
  }, [afterSeq, filter, manager, onEvent, selectedNamespace]);
}
