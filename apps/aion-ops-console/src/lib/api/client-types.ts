import type {
  ActivityId,
  InterventionCapabilities,
  InterventionKind,
  Namespace,
  WorkflowId,
} from '@/types';

import type { JsonRecord } from './client-normalize';
import type { ApiCredentials } from './client-transport';

/** Start-workflow inputs (camelCase); the body is built server-shaped by ApiClient. */
export type StartWorkflowParams = {
  workflowType: string;
  /** Plain JSON input, auto-wrapped server-side as an `application/json` payload. */
  input?: JsonRecord | undefined;
  /** Optional R-4 steered-start routing key. */
  routingKey?: string | undefined;
  /** Empty/absent selects the namespace's default activity task queue. */
  taskQueue?: string | undefined;
};

/** One addressable activity attempt and the controls its worker supports. */
export type AttemptCapabilities = {
  activityId: ActivityId;
  attempt: number;
  capabilities: InterventionCapabilities;
};

/** Target attempt plus the neutral intervention primitive. */
export type InterveneParams = {
  workflowId: WorkflowId;
  activityId: ActivityId;
  attempt: number;
  kind: InterventionKind;
};

export type FetchFn = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

export type ApiClientOptions = {
  baseUrl?: string;
  fetchImpl?: FetchFn;
  credentials?: ApiCredentials;
};

export type RequestOptions = {
  namespace: Namespace;
  credentials?: ApiCredentials | undefined;
  signal?: AbortSignal | undefined;
};

export type WorkflowPageRequest = {
  cursor?: string | undefined;
  limit?: number | undefined;
};

/** Field-aware event-search fields, AND-combined server-side. */
export type EventSearchQuery = {
  eventType?: string;
  workflowType?: string;
  activityType?: string;
  errorText?: string;
  recordedAfter?: string;
  recordedBefore?: string;
};

/** Request-time authorization capabilities returned by `GET /whoami`. */
export type Capabilities = {
  subject: string;
  authEnabled: boolean;
  deployGranted: boolean;
  allNamespaces: boolean;
  namespaces: Namespace[];
};

/** Raw `/whoami` envelope (server snake_case). */
export type WhoAmIResponse = {
  subject?: unknown;
  auth_enabled?: unknown;
  deploy_granted?: unknown;
  all_namespaces?: unknown;
  namespaces?: unknown;
};

export type RequestBody = JsonRecord | undefined;
