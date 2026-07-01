import type { ApiClientOptions } from '@/lib/api';
import { ApiClient } from '@/lib/api';
import { AionClusterStreamManager } from '@/lib/api/cluster-stream';
import { AionEventWebSocketManager, type SocketCredentials } from '@/lib/api/websocket';
import type { Namespace } from '@/types';

import { buildCredentials, getOpsConsoleConfig, type OpsConsoleConfig } from './env';

/**
 * Single source of configured clients. Every call site that previously did a
 * bare `new ApiClient()` (which sent no credentials and got `namespace_denied`)
 * now goes through here so the base URL and `x-aion-*` credentials from
 * {@link OpsConsoleConfig} are always applied.
 */
export type ConfiguredClientOptions = {
  /** Override the config (tests); defaults to the live env-derived config. */
  config?: OpsConsoleConfig;
  /** Selected namespace — scopes `x-aion-namespaces` to what the user is viewing. */
  namespace?: Namespace | null;
  /** Override the REST base URL (failover own-read repoint to a survivor node). */
  baseUrl?: string;
  /** Override the fetch implementation (tests); defaults to the global `fetch`. */
  fetchImpl?: ApiClientOptions['fetchImpl'];
};

/**
 * Build an {@link ApiClient} bound to the configured base URL and credentials.
 * Pass `baseUrl` to repoint a single client at a specific node (failover reads);
 * pass `namespace` to scope the namespace header to the selected namespace.
 */
export function createConfiguredApiClient(options: ConfiguredClientOptions = {}): ApiClient {
  const config = options.config ?? getOpsConsoleConfig();
  const credentials = buildCredentials(config, options.namespace);

  return new ApiClient({
    baseUrl: options.baseUrl ?? config.apiBaseUrl,
    ...(credentials === undefined ? {} : { credentials }),
    ...(options.fetchImpl === undefined ? {} : { fetchImpl: options.fetchImpl }),
  });
}

/**
 * Build the live-events WebSocket manager bound to the configured WS base URL.
 *
 * A browser cannot set `x-aion-*` headers on a WebSocket handshake, so the
 * caller's credentials authorize the socket as query parameters instead (the
 * server promotes them back to headers — see `WsCaller` in aion-server). The
 * connection authorizes the *full* configured namespace set so switching the
 * selected namespace re-filters the subscription without reconnecting; the
 * per-subscription filter narrows within the authorized set, it does not
 * authorize on its own.
 */
export function createConfiguredWebSocketManager(
  config: OpsConsoleConfig = getOpsConsoleConfig()
): AionEventWebSocketManager {
  const credentials: SocketCredentials = {
    namespaces: config.namespaces,
    ...(config.subject === undefined ? {} : { subject: config.subject }),
    ...(config.bearerToken === undefined ? {} : { bearerToken: config.bearerToken }),
  };

  return new AionEventWebSocketManager({ baseUrl: config.wsBaseUrl, credentials });
}

/** The app-wide singleton WS manager, bound to the configured WS base URL. */
export const configuredEventSocket = createConfiguredWebSocketManager();

/**
 * Build the WS3 cluster-stream manager bound to the configured WS base URL.
 *
 * This opens a DEDICATED `/events/stream` socket carrying the cluster
 * subscription arm (the server keeps the socket one-subscription-per-socket, so
 * the cluster channel is its own socket, not a multiplexed second subscription
 * on the workflow socket). Cluster topology is deployment-scoped and deploy-gated
 * server-side, but the grant is NOT baked into the bundle: in auth-off
 * single-tenant operator mode the server grants the socket server-side at the
 * handshake (no `x-aion-deploy` param needed), and under real auth the grant
 * lives in the bearer token's `deploy` claim. The console therefore opens the
 * socket with no compiled deploy flag; whether topology flows is the server's
 * runtime decision (also reflected in `GET /whoami`).
 */
export function createConfiguredClusterStream(
  config: OpsConsoleConfig = getOpsConsoleConfig()
): AionClusterStreamManager {
  const credentials: SocketCredentials = {
    namespaces: config.namespaces,
    ...(config.subject === undefined ? {} : { subject: config.subject }),
    ...(config.bearerToken === undefined ? {} : { bearerToken: config.bearerToken }),
  };

  return new AionClusterStreamManager({ baseUrl: config.wsBaseUrl, credentials });
}

/** The app-wide singleton cluster-stream manager. */
export const configuredClusterStream = createConfiguredClusterStream();
