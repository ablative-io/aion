import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { type ReactNode, useEffect, useState } from 'react';

import { NamespaceProvider } from '@/features/namespace';
import { aionEventSocket, type AionEventWebSocketManager, type ApiClient } from '@/lib/api';
import type { Namespace } from '@/types';

const QUERY_STALE_TIME_MS = 30_000;
const QUERY_GC_TIME_MS = 5 * 60_000;

export type AppProvidersProps = {
  apiClient?: Pick<ApiClient, 'listNamespaces'>;
  children: ReactNode;
  initialNamespace?: Namespace;
  queryClient?: QueryClient;
  websocketManager?: Pick<AionEventWebSocketManager, 'close' | 'connect'>;
};

export function AppProviders({
  apiClient,
  children,
  initialNamespace,
  queryClient,
  websocketManager = aionEventSocket,
}: AppProvidersProps) {
  const [client] = useState(() => queryClient ?? createDashboardQueryClient());

  return (
    <QueryClientProvider client={client}>
      <NamespaceProvider apiClient={apiClient} initialNamespace={initialNamespace}>
        <WebSocketConnection manager={websocketManager}>{children}</WebSocketConnection>
      </NamespaceProvider>
    </QueryClientProvider>
  );
}

function WebSocketConnection({
  children,
  manager,
}: {
  children: ReactNode;
  manager: Pick<AionEventWebSocketManager, 'close' | 'connect'>;
}) {
  useEffect(() => {
    manager.connect();

    return () => {
      manager.close();
    };
  }, [manager]);

  return <>{children}</>;
}

export function createDashboardQueryClient() {
  return new QueryClient({
    defaultOptions: {
      queries: {
        gcTime: QUERY_GC_TIME_MS,
        refetchOnWindowFocus: false,
        retry: 1,
        staleTime: QUERY_STALE_TIME_MS,
      },
    },
  });
}
