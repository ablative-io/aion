import { useQuery, type UseQueryResult } from '@tanstack/react-query';
import { createContext, type ReactNode, useContext, useEffect, useMemo, useState } from 'react';

import { ApiClient, type ApiCredentials } from '@/lib/api';
import type { Namespace } from '@/types';

export const namespaceQueryKey = ['namespaces'] as const;

export type NamespaceQueryState = Pick<
  UseQueryResult<Namespace[], Error>,
  'error' | 'isError' | 'isLoading' | 'isFetching' | 'refetch'
>;

export type NamespaceContextValue = NamespaceQueryState & {
  namespaces: Namespace[];
  selectedNamespace: Namespace | null;
  setSelectedNamespace: (namespace: Namespace) => void;
};

type NamespaceProviderProps = {
  children: ReactNode;
  apiClient?: Pick<ApiClient, 'listNamespaces'>;
  credentials?: ApiCredentials;
  initialNamespace?: Namespace;
};

const defaultApiClient = new ApiClient();
const NamespaceContext = createContext<NamespaceContextValue | null>(null);

export function NamespaceProvider({
  apiClient = defaultApiClient,
  children,
  credentials,
  initialNamespace,
}: NamespaceProviderProps) {
  const [selectedNamespace, setSelectedNamespace] = useState<Namespace | null>(
    initialNamespace ?? null
  );
  const namespaceQuery = useQuery<Namespace[], Error>({
    queryKey: namespaceQueryKey,
    queryFn: () => apiClient.listNamespaces({ credentials }),
  });
  const namespaces = namespaceQuery.data ?? [];

  useEffect(() => {
    if (selectedNamespace !== null || namespaces.length === 0) {
      return;
    }

    const [firstNamespace] = namespaces;
    if (firstNamespace !== undefined) {
      setSelectedNamespace(firstNamespace);
    }
  }, [namespaces, selectedNamespace]);

  useEffect(() => {
    if (selectedNamespace === null || namespaces.length === 0) {
      return;
    }

    if (!namespaces.includes(selectedNamespace)) {
      const [firstNamespace] = namespaces;
      setSelectedNamespace(firstNamespace ?? null);
    }
  }, [namespaces, selectedNamespace]);

  const value = useMemo<NamespaceContextValue>(
    () => ({
      error: namespaceQuery.error,
      isError: namespaceQuery.isError,
      isFetching: namespaceQuery.isFetching,
      isLoading: namespaceQuery.isLoading,
      namespaces,
      refetch: namespaceQuery.refetch,
      selectedNamespace,
      setSelectedNamespace,
    }),
    [
      namespaceQuery.error,
      namespaceQuery.isError,
      namespaceQuery.isFetching,
      namespaceQuery.isLoading,
      namespaceQuery.refetch,
      namespaces,
      selectedNamespace,
    ]
  );

  return <NamespaceContext.Provider value={value}>{children}</NamespaceContext.Provider>;
}

export function useNamespace(): NamespaceContextValue {
  const value = useContext(NamespaceContext);

  if (value === null) {
    throw new Error('useNamespace must be used within a NamespaceProvider');
  }

  return value;
}

export function selectNamespace(nextNamespace: Namespace): Namespace {
  return nextNamespace;
}

export function applyNamespaceSelection(
  setSelectedNamespace: (namespace: Namespace) => void,
  nextNamespace: Namespace
) {
  setSelectedNamespace(selectNamespace(nextNamespace));
}
