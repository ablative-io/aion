import { type UseQueryResult, useQuery } from '@tanstack/react-query';
import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
} from 'react';

import type { ApiClient, ApiCredentials } from '@/lib/api';
import { buildCredentials, createConfiguredApiClient, getDashboardConfig } from '@/lib/config';
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
  apiClient?: Pick<ApiClient, 'listNamespaces'> | undefined;
  credentials?: ApiCredentials | undefined;
  initialNamespace?: Namespace | undefined;
};

const NamespaceContext = createContext<NamespaceContextValue | null>(null);

export function NamespaceProvider({
  apiClient,
  children,
  credentials,
  initialNamespace,
}: NamespaceProviderProps) {
  const [selectedNamespace, setSelectedNamespaceState] = useState<Namespace | null>(
    isSelectedNamespace(initialNamespace) ? initialNamespace : null
  );
  // listNamespaces must carry credentials (x-aion-namespaces) or the server
  // replies namespace_denied. The env-derived config supplies the grant; an
  // explicit `credentials` prop (tests) overrides it.
  const listCredentials = useMemo<ApiCredentials | undefined>(
    () => credentials ?? buildCredentials(getDashboardConfig()),
    [credentials]
  );
  const client = useMemo<Pick<ApiClient, 'listNamespaces'>>(
    () => apiClient ?? createConfiguredApiClient(),
    [apiClient]
  );
  const namespaceQuery = useQuery<Namespace[], Error>({
    queryKey: namespaceQueryKey,
    queryFn: () => client.listNamespaces({ credentials: listCredentials }),
  });
  const namespaces = namespaceQuery.data ?? [];

  const setSelectedNamespace = useCallback(
    (namespace: Namespace) => {
      setSelectedNamespaceState(selectNamespace(namespace, namespaces));
    },
    [namespaces]
  );

  useEffect(() => {
    if (selectedNamespace !== null || namespaces.length === 0) {
      return;
    }

    const [firstNamespace] = namespaces;
    if (firstNamespace !== undefined) {
      setSelectedNamespaceState(firstNamespace);
    }
  }, [namespaces, selectedNamespace]);

  useEffect(() => {
    if (selectedNamespace === null || namespaces.length === 0) {
      return;
    }

    if (!namespaces.includes(selectedNamespace)) {
      const [firstNamespace] = namespaces;
      setSelectedNamespaceState(firstNamespace ?? null);
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
      setSelectedNamespace,
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

export function isSelectedNamespace(
  namespace: Namespace | null | undefined
): namespace is Namespace {
  return typeof namespace === 'string' && namespace.trim().length > 0;
}

export function requireSelectedNamespace(
  namespace: Namespace | null | undefined,
  surface: string
): Namespace {
  if (!isSelectedNamespace(namespace)) {
    throw new Error(`A namespace must be selected before ${surface}.`);
  }

  return namespace;
}

export function selectNamespace(
  nextNamespace: Namespace,
  availableNamespaces: readonly Namespace[] = []
): Namespace {
  const namespace = requireSelectedNamespace(nextNamespace, 'selecting a namespace');

  if (availableNamespaces.length > 0 && !availableNamespaces.includes(namespace)) {
    throw new Error(`Namespace ${namespace} is not available in the loaded namespace list.`);
  }

  return namespace;
}

export function applyNamespaceSelection(
  setSelectedNamespace: (namespace: Namespace) => void,
  nextNamespace: Namespace,
  availableNamespaces: readonly Namespace[] = []
) {
  setSelectedNamespace(selectNamespace(nextNamespace, availableNamespaces));
}
