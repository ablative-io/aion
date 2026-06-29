import type { ApiClientOptions } from '@/lib/api';
import { ApiClient } from '@/lib/api';
import { AionEventWebSocketManager } from '@/lib/api/websocket';
import type { Namespace } from '@/types';

import { buildCredentials, type DashboardConfig, getDashboardConfig } from './env';

/**
 * Single source of configured clients. Every call site that previously did a
 * bare `new ApiClient()` (which sent no credentials and got `namespace_denied`)
 * now goes through here so the base URL and `x-aion-*` credentials from
 * {@link DashboardConfig} are always applied.
 */
export type ConfiguredClientOptions = {
  /** Override the config (tests); defaults to the live env-derived config. */
  config?: DashboardConfig;
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
  const config = options.config ?? getDashboardConfig();
  const credentials = buildCredentials(config, options.namespace);

  return new ApiClient({
    baseUrl: options.baseUrl ?? config.apiBaseUrl,
    ...(credentials === undefined ? {} : { credentials }),
    ...(options.fetchImpl === undefined ? {} : { fetchImpl: options.fetchImpl }),
  });
}

/**
 * Build the live-events WebSocket manager bound to the configured WS base URL.
 * The browser carries cookies/origin; `x-aion-*` headers cannot be set on a WS
 * handshake from the browser, so namespace scoping for the stream is enforced by
 * the per-subscription `namespace` filter, not a header.
 */
export function createConfiguredWebSocketManager(
  config: DashboardConfig = getDashboardConfig()
): AionEventWebSocketManager {
  return new AionEventWebSocketManager({ baseUrl: config.wsBaseUrl });
}

/** The app-wide singleton WS manager, bound to the configured WS base URL. */
export const configuredEventSocket = createConfiguredWebSocketManager();
