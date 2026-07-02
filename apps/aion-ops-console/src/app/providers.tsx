import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { type ReactNode, useEffect, useState } from 'react';

import { CapabilitiesProvider } from '@/features/capabilities';
import { NamespaceProvider } from '@/features/namespace';
import type { AionEventWebSocketManager, ApiClient } from '@/lib/api';
import { configuredEventSocket } from '@/lib/config';
import { KeybindingsProvider } from '@/lib/keybindings';
import type { Namespace } from '@/types';

const QUERY_STALE_TIME_MS = 30_000;
const QUERY_GC_TIME_MS = 5 * 60_000;

export type AppProvidersProps = {
  apiClient?: Pick<ApiClient, 'listNamespaces' | 'getCapabilities'> | undefined;
  children: ReactNode;
  initialNamespace?: Namespace | undefined;
  queryClient?: QueryClient | undefined;
  websocketManager?: Pick<AionEventWebSocketManager, 'close' | 'connect'> | undefined;
};

export function AppProviders({
  apiClient,
  children,
  initialNamespace,
  queryClient,
  websocketManager = configuredEventSocket,
}: AppProvidersProps) {
  const [client] = useState(() => queryClient ?? createOpsConsoleQueryClient());

  return (
    <QueryClientProvider client={client}>
      {/* Keybindings sit above the router: the single global key listener and
          the registry survive navigation, like the WS manager (M3). */}
      <KeybindingsProvider>
        <CapabilitiesProvider apiClient={apiClient}>
          <NamespaceProvider apiClient={apiClient} initialNamespace={initialNamespace}>
            <WebSocketConnection manager={websocketManager}>{children}</WebSocketConnection>
          </NamespaceProvider>
        </CapabilitiesProvider>
      </KeybindingsProvider>
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
  // M3: this component is mounted once above the router and never remounts on
  // navigation, so the single WS manager connects once and is torn down only on
  // full app unmount — route churn does not close or re-open the socket. The
  // effect dep is the stable manager identity, never anything route-derived.
  useEffect(() => connectWebSocketLifecycle(manager), [manager]);

  return <>{children}</>;
}

/**
 * Connect the shared WS manager and return its teardown. Extracted so the M3
 * invariant (connect once on mount, close only on full unmount, never on route
 * change) is unit-testable without a DOM: the effect's dependency is the stable
 * manager identity, so this runs exactly once per app mount.
 */
export function connectWebSocketLifecycle(
  manager: Pick<AionEventWebSocketManager, 'close' | 'connect'>
): () => void {
  manager.connect();

  return () => {
    manager.close();
  };
}

export function createOpsConsoleQueryClient() {
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
