import type { Namespace } from '@/types';

// AW REST contract surface: update this one object when cluster AW pins endpoint
// paths, methods, request body keys, pagination names, or envelope response
// shapes. Kept out of client.ts so that file stays under the house LOC limit.
export const AW_REST_CONTRACT = {
  endpoints: {
    workflows: '/workflows/list',
    workflowsPlain: '/workflows',
    workflowsCount: '/workflows/count',
    history: '/workflows/describe',
    namespaces: '/namespaces',
    // `POST /namespaces` is the explicit operator namespace-create; it shares the
    // `/namespaces` path with the list `GET`, distinguished by method below.
    namespaceCreate: '/namespaces',
    namespaceRecords: '/namespaces/records',
    // `{name}` is substituted with the URL-encoded namespace at call time; this
    // is the auth-scoped quorum-CAS `PUT` that sets a namespace's placement.
    namespacePlacement: '/namespaces/{name}/placement',
    whoami: '/whoami',
    eventSearch: '/events/search',
    clusterCommand: '/cluster/command',
    workflowStart: '/workflows/start',
    deployPackages: '/deploy/packages',
    deployVersions: '/deploy/versions',
    deployRoute: '/deploy/route',
    deployUnload: '/deploy/unload',
  },
  methods: {
    workflows: 'POST',
    history: 'POST',
    namespaces: 'GET',
    namespaceCreate: 'POST',
    namespaceRecords: 'GET',
    namespacePlacement: 'PUT',
    whoami: 'GET',
    workflowsPlain: 'GET',
    workflowsCount: 'GET',
    eventSearch: 'POST',
    workflowStart: 'POST',
    deployPackages: 'POST',
    deployVersions: 'GET',
    deployRoute: 'POST',
    deployUnload: 'POST',
  },
  requestKeys: {
    namespace: 'namespace',
    // `POST /namespaces` body key — the free-form namespace name to create.
    namespaceName: 'name',
    filter: 'filter',
    workflowId: 'workflow_id',
    runId: 'run_id',
    includeHistory: 'include_history',
    query: 'query',
    pagination: {
      cursor: 'cursor',
      limit: 'limit',
    },
  },
  responseKeys: {
    items: 'items',
    summaries: 'summaries',
    events: 'events',
    history: 'history',
    namespaces: 'namespaces',
    results: 'results',
    nextCursor: 'next_cursor',
    hasMore: 'has_more',
    payload: 'payload',
    payloadBytes: 'bytes',
  },
} as const;

export type ApiCredentials = {
  bearerToken?: string;
  subject?: string;
  namespaces?: readonly Namespace[];
  headers?: HeadersInit;
};

export function buildUrl(baseUrl: string, path: string): string {
  return `${baseUrl}${path}`;
}

export function stripTrailingSlash(value: string): string {
  return value.endsWith('/') ? value.slice(0, -1) : value;
}

/**
 * Coerce an uploaded archive to a {@link BodyInit}. A `Uint8Array` is sent as its
 * backing `ArrayBuffer` slice (a typed array view is not itself `BodyInit` under
 * the strict DOM lib types); `ArrayBuffer` and `Blob` pass through unchanged.
 */
export function toBinaryBody(archive: ArrayBuffer | Uint8Array | Blob): BodyInit {
  if (archive instanceof Uint8Array) {
    return archive.slice().buffer;
  }

  return archive;
}

export function mergeCredentials(
  base: ApiCredentials | undefined,
  override: ApiCredentials | undefined
): ApiCredentials | undefined {
  if (base === undefined) {
    return override;
  }

  if (override === undefined) {
    return base;
  }

  return {
    ...base,
    ...override,
    headers: mergeHeaderInputs(base.headers, override.headers),
  };
}

function mergeHeaderInputs(
  base: HeadersInit | undefined,
  override: HeadersInit | undefined
): Headers {
  const headers = new Headers(base);
  appendHeaders(headers, override);
  return headers;
}

export function appendHeaders(headers: Headers, input: HeadersInit | undefined): void {
  if (input === undefined) {
    return;
  }

  new Headers(input).forEach((value, key) => {
    headers.set(key, value);
  });
}
