import { type UseQueryResult, useQuery } from '@tanstack/react-query';
import { createContext, type ReactNode, useContext, useMemo } from 'react';

import type { ApiClient, Capabilities } from '@/lib/api';
import { buildCredentials, createConfiguredApiClient, getOpsConsoleConfig } from '@/lib/config';

export const capabilitiesQueryKey = ['capabilities'] as const;

export type CapabilitiesQueryState = Pick<
  UseQueryResult<Capabilities, Error>,
  'error' | 'isError' | 'isLoading' | 'isFetching' | 'refetch'
>;

export type CapabilitiesContextValue = CapabilitiesQueryState & {
  /**
   * The caller's runtime capabilities, or `null` until the first `/whoami`
   * response. Affordances gate on this (never on a build-time flag): while it is
   * `null` the console treats the caller as UNPRIVILEGED, so a missing/slow
   * server can never momentarily expose a deploy affordance.
   */
  capabilities: Capabilities | null;
  /** Whether the resolved caller may deploy. `false` until `/whoami` resolves. */
  deployGranted: boolean;
};

type CapabilitiesProviderProps = {
  children: ReactNode;
  /** Injected client (tests); defaults to the env-configured client. */
  apiClient?: Pick<ApiClient, 'getCapabilities'> | undefined;
};

const CapabilitiesContext = createContext<CapabilitiesContextValue | null>(null);

export function CapabilitiesProvider({ apiClient, children }: CapabilitiesProviderProps) {
  // /whoami runs through the same credential path as every request. In auth-off
  // operator mode no credentials are needed (the server grants full access); the
  // env-derived credentials carry the bearer/subject under real auth.
  const credentials = useMemo(() => buildCredentials(getOpsConsoleConfig()), []);
  const client = useMemo<Pick<ApiClient, 'getCapabilities'>>(
    () => apiClient ?? createConfiguredApiClient(),
    [apiClient]
  );
  const query = useQuery<Capabilities, Error>({
    queryKey: capabilitiesQueryKey,
    queryFn: () => client.getCapabilities({ credentials }),
  });

  const capabilities = query.data ?? null;
  const value = useMemo<CapabilitiesContextValue>(
    () => ({
      capabilities,
      deployGranted: capabilities?.deployGranted ?? false,
      error: query.error,
      isError: query.isError,
      isFetching: query.isFetching,
      isLoading: query.isLoading,
      refetch: query.refetch,
    }),
    [capabilities, query.error, query.isError, query.isFetching, query.isLoading, query.refetch]
  );

  return <CapabilitiesContext.Provider value={value}>{children}</CapabilitiesContext.Provider>;
}

export function useCapabilities(): CapabilitiesContextValue {
  const value = useContext(CapabilitiesContext);

  if (value === null) {
    throw new Error('useCapabilities must be used within a CapabilitiesProvider');
  }

  return value;
}
