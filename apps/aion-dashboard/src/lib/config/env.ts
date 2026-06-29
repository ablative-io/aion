import type { ApiCredentials } from '@/lib/api';
import type { Namespace } from '@/types';

/**
 * Live-cluster wiring config, parsed once from the Vite environment.
 *
 * Two gaps surfaced in the live rehearsal motivate this module:
 *  1. Bare `new ApiClient()` sent no `x-aion-namespaces`, so the server replied
 *     `namespace_denied`. The credentials built here are threaded through every
 *     client so the namespace header is always present.
 *  2. The failover view hardcoded namespace `demo`; the demo cluster uses
 *     `default`. The default namespace now comes from config, not a literal.
 *
 * No secrets are baked in: the bearer token / subject come from the environment
 * at build time (or are simply absent in dev where auth is disabled).
 */
export type DashboardConfig = {
  /** REST base URL (no trailing slash). Empty string = same origin. */
  apiBaseUrl: string;
  /** WebSocket base URL (no trailing slash). Empty string = derive from origin. */
  wsBaseUrl: string;
  /** Namespaces granted to the caller; first entry is the default selection. */
  namespaces: readonly Namespace[];
  /** Optional `x-aion-subject` value (dev auth). */
  subject?: string;
  /** Optional bearer token for `authorization` (JWT auth). */
  bearerToken?: string;
};

/** The subset of `import.meta.env` this module reads; injectable for tests. */
export type DashboardEnv = {
  VITE_AION_API_BASE?: string;
  VITE_AION_WS_BASE?: string;
  VITE_AION_NAMESPACES?: string;
  VITE_AION_SUBJECT?: string;
  VITE_AION_BEARER_TOKEN?: string;
};

/** Localhost defaults matching `scripts/demo/demo-failover.sh` node 0. */
export const DEFAULT_API_BASE_URL = 'http://127.0.0.1:8090';
export const DEFAULT_WS_BASE_URL = 'ws://127.0.0.1:8090';

/**
 * Parse the Vite environment into a {@link DashboardConfig}.
 *
 * - `apiBaseUrl` falls back to the localhost demo node when unset/blank.
 * - `wsBaseUrl` falls back to the configured WS base, else is derived from the
 *   API base by swapping the scheme (`http`→`ws`, `https`→`wss`), else the
 *   localhost WS default.
 * - Namespaces are split on commas with blanks dropped; an empty list is a
 *   visible config gap, not a silent default.
 */
export function parseDashboardConfig(env: DashboardEnv): DashboardConfig {
  const apiBaseUrl = stripTrailingSlash(nonBlank(env.VITE_AION_API_BASE) ?? DEFAULT_API_BASE_URL);
  const wsBaseUrl = stripTrailingSlash(resolveWsBaseUrl(env.VITE_AION_WS_BASE, apiBaseUrl));
  const namespaces = parseNamespaceList(env.VITE_AION_NAMESPACES);
  const subject = nonBlank(env.VITE_AION_SUBJECT);
  const bearerToken = nonBlank(env.VITE_AION_BEARER_TOKEN);

  return {
    apiBaseUrl,
    wsBaseUrl,
    namespaces,
    ...(subject === undefined ? {} : { subject }),
    ...(bearerToken === undefined ? {} : { bearerToken }),
  };
}

/**
 * Build the {@link ApiCredentials} the client sends. When `namespace` is given
 * (the selected namespace) it takes precedence so requests are scoped to exactly
 * the namespace the user is viewing; otherwise the full configured grant is sent.
 * Returns `undefined` when there is nothing to send so the client stays bare in
 * truly unconfigured setups.
 */
export function buildCredentials(
  config: DashboardConfig,
  namespace?: Namespace | null
): ApiCredentials | undefined {
  const namespaces = resolveCredentialNamespaces(config.namespaces, namespace);
  const credentials: ApiCredentials = {};

  if (namespaces.length > 0) {
    credentials.namespaces = namespaces;
  }
  if (config.subject !== undefined) {
    credentials.subject = config.subject;
  }
  if (config.bearerToken !== undefined) {
    credentials.bearerToken = config.bearerToken;
  }

  return Object.keys(credentials).length === 0 ? undefined : credentials;
}

function resolveCredentialNamespaces(
  granted: readonly Namespace[],
  selected: Namespace | null | undefined
): readonly Namespace[] {
  if (typeof selected === 'string' && selected.trim().length > 0) {
    const trimmed = selected.trim() as Namespace;
    return granted.includes(trimmed) || granted.length === 0 ? [trimmed] : [trimmed, ...granted];
  }

  return granted;
}

function resolveWsBaseUrl(wsBase: string | undefined, apiBaseUrl: string): string {
  const explicit = nonBlank(wsBase);
  if (explicit !== undefined) {
    return explicit;
  }

  if (apiBaseUrl.startsWith('https://')) {
    return `wss://${apiBaseUrl.slice('https://'.length)}`;
  }
  if (apiBaseUrl.startsWith('http://')) {
    return `ws://${apiBaseUrl.slice('http://'.length)}`;
  }

  return DEFAULT_WS_BASE_URL;
}

function parseNamespaceList(raw: string | undefined): readonly Namespace[] {
  if (raw === undefined) {
    return [];
  }

  return raw
    .split(',')
    .map((entry) => entry.trim())
    .filter((entry) => entry.length > 0) as Namespace[];
}

function nonBlank(value: string | undefined): string | undefined {
  if (value === undefined) {
    return undefined;
  }

  const trimmed = value.trim();
  return trimmed.length === 0 ? undefined : trimmed;
}

function stripTrailingSlash(value: string): string {
  return value.endsWith('/') ? value.slice(0, -1) : value;
}

let cachedConfig: DashboardConfig | null = null;

/**
 * The live {@link DashboardConfig} read from `import.meta.env`, parsed once and
 * memoised. Components/hooks read this; tests call {@link parseDashboardConfig}
 * with an explicit env so they never touch the ambient build environment.
 */
export function getDashboardConfig(): DashboardConfig {
  if (cachedConfig === null) {
    cachedConfig = parseDashboardConfig(import.meta.env);
  }

  return cachedConfig;
}
