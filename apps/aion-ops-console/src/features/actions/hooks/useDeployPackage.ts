import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useMemo } from 'react';

import type { ApiClient, LoadPackageResult, WorkflowVersion } from '@/lib/api';
import { createConfiguredApiClient } from '@/lib/config';

export const deployVersionsQueryKey = ['deploy', 'versions'] as const;

export type DeployClient = Pick<ApiClient, 'deployPackage' | 'listVersions'>;

export type UseDeployOptions = {
  /** Injected client (tests); defaults to the configured deploy-scoped client. */
  apiClient?: DeployClient | undefined;
  /** Gate the versions query (e.g. only fetch when the panel is mounted). */
  enabled?: boolean | undefined;
};

function resolveClient(apiClient: DeployClient | undefined): DeployClient {
  // Deploy is deployment-scoped (no namespace); the configured client still
  // carries the bearer/subject + deploy grant from config.
  return apiClient ?? createConfiguredApiClient();
}

/**
 * Query the loaded package versions (`GET /deploy/versions`). Deployment-scoped.
 * When the cluster runs with `[deploy] enabled=false` the server returns a real
 * 404 which propagates as `query.error` — the panel surfaces "deploy disabled"
 * honestly rather than rendering an empty table as if it had succeeded.
 */
export function useWorkflowVersions({ apiClient, enabled = true }: UseDeployOptions = {}) {
  const client = useMemo<DeployClient>(() => resolveClient(apiClient), [apiClient]);

  return useQuery<WorkflowVersion[]>({
    enabled,
    queryKey: deployVersionsQueryKey,
    queryFn: () => client.listVersions(),
  });
}

export type DeployPackageVariables = {
  archive: ArrayBuffer | Uint8Array | Blob;
};

/**
 * Mutation that uploads a `.aion` package archive (`POST /deploy/packages`).
 *
 * Resolves with the server's real {@link LoadPackageResult} (content hash, route
 * change, idempotency flag) — the view reports success only on that confirmed
 * response. On success the versions query is invalidated so the new version
 * appears. Failures (404 deploy-disabled, 403 deploy_denied, 413 oversized, 400
 * malformed archive) surface as the mutation `error`.
 */
export function useDeployPackage({ apiClient }: UseDeployOptions = {}) {
  const queryClient = useQueryClient();
  const client = useMemo<DeployClient>(() => resolveClient(apiClient), [apiClient]);

  return useMutation<LoadPackageResult, Error, DeployPackageVariables>({
    mutationFn: (variables) => client.deployPackage(variables.archive),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: deployVersionsQueryKey });
    },
  });
}
