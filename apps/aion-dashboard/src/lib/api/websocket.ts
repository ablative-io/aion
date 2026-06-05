import type { Event, Namespace } from '@/types';

export type NamespaceScopedSubscriptionFilter = {
  namespace: Namespace;
  afterSeq?: number;
};

export type EventHandler = (event: Event) => void;
export type Unsubscribe = () => void;

export type EventSubscriptionManager = {
  subscribe: <TFilter extends NamespaceScopedSubscriptionFilter>(
    filter: TFilter,
    handler: EventHandler
  ) => Unsubscribe;
};

export const aionWebSocketManager: EventSubscriptionManager = {
  subscribe() {
    return () => undefined;
  },
};
