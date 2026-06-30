/// <reference types="vite/client" />

/**
 * Typed Vite environment for the dashboard. These are the only `VITE_*` keys the
 * app reads; declaring them as `string | undefined` keeps {@link import.meta.env}
 * access type-safe (no implicit `any` from Vite's fallback index signature) and
 * documents the live-cluster wiring contract in one place.
 *
 * See {@link "@/lib/config/env"} for how these are parsed into a OpsConsoleConfig.
 */
interface ImportMetaEnv {
  /** REST base URL, e.g. `http://127.0.0.1:8090`. Empty/unset = same origin. */
  readonly VITE_AION_API_BASE?: string;
  /** WebSocket base URL, e.g. `ws://127.0.0.1:8090`. Empty/unset = derived from origin. */
  readonly VITE_AION_WS_BASE?: string;
  /** Comma-separated namespaces granted to the caller, e.g. `default,ops`. */
  readonly VITE_AION_NAMESPACES?: string;
  /** Optional caller subject for `x-aion-subject` (dev auth). */
  readonly VITE_AION_SUBJECT?: string;
  /** Optional bearer token for `authorization: Bearer <token>` (JWT auth). */
  readonly VITE_AION_BEARER_TOKEN?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
