import { expect, test } from 'bun:test';

import type { Event, WorkflowFilter, WorkflowSummary } from '@/types';

import { ApiClient, ApiError } from './client';

const namespace = 'default';
const workflowId = '00000000-0000-0000-0000-000000000001';

const filter: WorkflowFilter = {
  workflow_type: null,
  status: null,
  started_after: null,
  started_before: null,
  parent: null,
};

const summary: WorkflowSummary = {
  workflow_id: workflowId,
  workflow_type: 'checkout',
  status: 'Running',
  started_at: '2026-06-05T20:00:00Z',
  ended_at: null,
  parent: null,
};

const event: Event = {
  type: 'WorkflowStarted',
  data: {
    envelope: {
      seq: 1,
      recorded_at: '2026-06-05T20:00:00Z',
      workflow_id: workflowId,
    },
    workflow_type: 'checkout',
    input: {
      content_type: 'Json',
      bytes: [123, 125],
    },
    run_id: '00000000-0000-0000-0000-0000000000a1',
    parent_run_id: null,
    package_version: '1.0.0',
  },
};

test('queryWorkflows posts a namespaced typed request and parses a workflow page', async () => {
  const calls: Request[] = [];
  const client = new ApiClient({
    baseUrl: 'https://aion.example',
    credentials: {
      bearerToken: 'token',
      subject: 'operator',
      namespaces: [namespace],
    },
    fetchImpl: async (input, init) => {
      const request = new Request(input, init);
      calls.push(request);

      return jsonResponse({ items: [summary], next_cursor: 'next', has_more: true });
    },
  });

  const page = await client.queryWorkflows(filter, { cursor: 'first', limit: 10 }, { namespace });

  expect(page).toEqual({ items: [summary], nextCursor: 'next', hasMore: true });
  expect(calls).toHaveLength(1);
  expect(calls[0]?.url).toBe('https://aion.example/workflows/list');
  expect(calls[0]?.headers.get('authorization')).toBe('Bearer token');
  expect(calls[0]?.headers.get('x-aion-subject')).toBe('operator');
  expect(calls[0]?.headers.get('x-aion-namespaces')).toBe(namespace);
  await expect(calls[0]?.json()).resolves.toEqual({
    namespace,
    filter,
    cursor: 'first',
    limit: 10,
  });
});

test('getHistory parses ordered typed events', async () => {
  const secondEvent: Event = {
    ...event,
    data: {
      ...event.data,
      envelope: {
        ...event.data.envelope,
        seq: 2,
      },
    },
  };
  const client = new ApiClient({
    fetchImpl: async () => jsonResponse({ history: [secondEvent, event] }),
  });

  const history = await client.getHistory(workflowId, { namespace });

  expect(history).toEqual([event, secondEvent]);
});

test('listNamespaces parses a typed namespace list', async () => {
  const client = new ApiClient({
    fetchImpl: async () => jsonResponse({ namespaces: ['default', 'ops'] }),
  });

  await expect(client.listNamespaces()).resolves.toEqual(['default', 'ops']);
});

test('listNamespaceRecords parses the placement projection (P2-I2 column source)', async () => {
  const client = new ApiClient({
    fetchImpl: async () =>
      jsonResponse([
        {
          name: 'orders',
          created_at: '2026-06-30T00:00:00Z',
          last_seen: '2026-06-30T01:00:00Z',
          origin: 'explicit',
          placement: { kind: 'pinned', nodes: ['node-a', 'node-b'] },
        },
        // A pre-Phase-2 row with NO placement key resolves to the unplaced default,
        // never a parse error (placement is a policy overlay, not identity).
        {
          name: 'legacy',
          created_at: '2026-06-29T00:00:00Z',
          last_seen: '2026-06-29T00:00:00Z',
          origin: 'worker_mint',
        },
      ]),
  });

  const records = await client.listNamespaceRecords();
  expect(records[0]?.placement).toEqual({ kind: 'pinned', nodes: ['node-a', 'node-b'] });
  expect(records[1]?.placement).toEqual({ kind: 'unplaced', nodes: [] });
});

test('setNamespacePlacement PUTs the target namespace path with the {kind, nodes} body', async () => {
  const calls: Request[] = [];
  const bodies: string[] = [];
  const client = new ApiClient({
    baseUrl: 'https://aion.example',
    credentials: { namespaces: ['orders'] },
    fetchImpl: async (input, init) => {
      const request = new Request(input, init);
      calls.push(request);
      bodies.push(typeof init?.body === 'string' ? init.body : '');
      return jsonResponse({ name: 'orders', created: false });
    },
  });

  await client.setNamespacePlacement('orders', { kind: 'pinned', nodes: ['node-a'] });

  expect(calls[0]?.method).toBe('PUT');
  expect(calls[0]?.url).toBe('https://aion.example/namespaces/orders/placement');
  // The grant travels on the namespace header so authorize_namespace passes.
  expect(calls[0]?.headers.get('x-aion-namespaces')).toBe('orders');
  expect(JSON.parse(bodies[0] ?? '{}')).toEqual({ kind: 'pinned', nodes: ['node-a'] });
});

test('setNamespacePlacement surfaces a server rejection as a typed ApiError', async () => {
  const client = new ApiClient({
    fetchImpl: async () =>
      jsonResponse(
        { code: 'not_found', message: 'namespace ghost does not exist' },
        { status: 404 }
      ),
  });

  await expect(
    client.setNamespacePlacement('ghost', { kind: 'unplaced', nodes: [] })
  ).rejects.toBeInstanceOf(ApiError);
});

test('createNamespace POSTs /namespaces with the {name} body and reports created', async () => {
  const calls: Request[] = [];
  const bodies: string[] = [];
  const client = new ApiClient({
    baseUrl: 'https://aion.example',
    credentials: { namespaces: ['orders'] },
    fetchImpl: async (input, init) => {
      const request = new Request(input, init);
      calls.push(request);
      bodies.push(typeof init?.body === 'string' ? init.body : '');
      return jsonResponse({ name: 'orders', created: true });
    },
  });

  await expect(client.createNamespace('orders')).resolves.toEqual({
    name: 'orders',
    created: true,
  });

  expect(calls[0]?.method).toBe('POST');
  expect(calls[0]?.url).toBe('https://aion.example/namespaces');
  // The grant travels on the namespace header so authorize_namespace passes.
  expect(calls[0]?.headers.get('x-aion-namespaces')).toBe('orders');
  expect(JSON.parse(bodies[0] ?? '{}')).toEqual({ name: 'orders' });
});

test('createNamespace reports the idempotent already-existed path honestly', async () => {
  const client = new ApiClient({
    fetchImpl: async () => jsonResponse({ name: 'orders', created: false }),
  });

  await expect(client.createNamespace('orders')).resolves.toEqual({
    name: 'orders',
    created: false,
  });
});

test('createNamespace surfaces an empty-name 400 as a typed ApiError', async () => {
  const client = new ApiClient({
    fetchImpl: async () =>
      jsonResponse(
        { code: 'invalid_input', message: 'namespace name must not be empty' },
        {
          status: 400,
        }
      ),
  });

  await expect(client.createNamespace('   ')).rejects.toBeInstanceOf(ApiError);
});

test('createNamespace surfaces a denied grant (403) as a typed ApiError', async () => {
  const client = new ApiClient({
    fetchImpl: async () =>
      jsonResponse({ code: 'PermissionDenied', message: 'namespace denied' }, { status: 403 }),
  });

  await expect(client.createNamespace('secret')).rejects.toBeInstanceOf(ApiError);
});

test('getCapabilities normalizes the /whoami operator-mode response', async () => {
  const calls: Request[] = [];
  const client = new ApiClient({
    baseUrl: 'https://aion.example',
    fetchImpl: async (input, init) => {
      calls.push(new Request(input, init));
      return jsonResponse({
        subject: 'operator',
        auth_enabled: false,
        deploy_granted: true,
        all_namespaces: true,
        namespaces: [],
      });
    },
  });

  await expect(client.getCapabilities()).resolves.toEqual({
    subject: 'operator',
    authEnabled: false,
    deployGranted: true,
    allNamespaces: true,
    namespaces: [],
  });
  expect(calls[0]?.url).toBe('https://aion.example/whoami');
  expect(calls[0]?.method).toBe('GET');
});

test('getCapabilities collapses a malformed /whoami response to least privilege', async () => {
  const client = new ApiClient({
    // Missing every field: auth treated as enabled, no deploy, no all-namespaces.
    fetchImpl: async () => jsonResponse({}),
  });

  await expect(client.getCapabilities()).resolves.toEqual({
    subject: 'anonymous',
    authEnabled: true,
    deployGranted: false,
    allNamespaces: false,
    namespaces: [],
  });
});

test('non-success responses throw ApiError with status, code, and message', async () => {
  const client = new ApiClient({
    fetchImpl: async () =>
      jsonResponse({ code: 'PermissionDenied', message: 'namespace denied' }, { status: 403 }),
  });

  try {
    await client.queryWorkflows(filter, {}, { namespace });
    throw new Error('expected queryWorkflows to throw');
  } catch (error) {
    expect(error).toBeInstanceOf(ApiError);
    expect((error as ApiError).status).toBe(403);
    expect((error as ApiError).code).toBe('PermissionDenied');
    expect((error as ApiError).message).toBe('namespace denied');
  }
});

test('malformed failure bodies still throw typed ApiError', async () => {
  const client = new ApiClient({
    fetchImpl: async () => new Response('not json', { status: 502 }),
  });

  try {
    await client.listNamespaces();
    throw new Error('expected listNamespaces to throw');
  } catch (error) {
    expect(error).toBeInstanceOf(ApiError);
    expect((error as ApiError).status).toBe(502);
    expect((error as ApiError).message).toBe('Request failed with 502');
  }
});

test('searchEvents posts a namespaced query and parses located results', async () => {
  const calls: Request[] = [];
  const client = new ApiClient({
    baseUrl: 'https://aion.example',
    fetchImpl: async (input, init) => {
      calls.push(new Request(input, init));
      return jsonResponse({
        results: [{ event, workflow_id: workflowId, seq: 1 }],
        next_cursor: null,
        has_more: false,
      });
    },
  });

  const page = await client.searchEvents(
    { eventType: 'WorkflowStarted', errorText: 'boom' },
    { limit: 25 },
    { namespace }
  );

  expect(page).toEqual({
    items: [{ event, workflowId, seq: 1 }],
    nextCursor: null,
    hasMore: false,
  });
  expect(calls[0]?.url).toBe('https://aion.example/events/search');
  await expect(calls[0]?.json()).resolves.toEqual({
    namespace,
    query: { eventType: 'WorkflowStarted', errorText: 'boom' },
    cursor: null,
    limit: 25,
  });
});

test('searchEvents surfaces an absent endpoint as ApiError (no silent empty result)', async () => {
  const client = new ApiClient({
    fetchImpl: async () => jsonResponse({ message: 'not found' }, { status: 404 }),
  });

  try {
    await client.searchEvents({ eventType: 'WorkflowStarted' }, {}, { namespace });
    throw new Error('expected searchEvents to throw');
  } catch (error) {
    expect(error).toBeInstanceOf(ApiError);
    expect((error as ApiError).status).toBe(404);
  }
});

test('default fetch path invokes the global fetch with correct receiver (no illegal invocation)', async () => {
  // Regression guard: storing the bare global `fetch` and calling it as
  // `this.fetchImpl(...)` strips its `this` binding and throws
  // `TypeError: Illegal invocation` in the browser. The client must default to a
  // properly-bound fetch. We replace globalThis.fetch with a spy (not passing
  // fetchImpl) so the default code path is exercised.
  const originalFetch = globalThis.fetch;
  const calls: string[] = [];

  // A naive global that throws if invoked with the wrong receiver, mirroring the
  // browser's illegal-invocation behaviour.
  const guardedFetch = function fetch(this: unknown, input: RequestInfo | URL): Promise<Response> {
    if (this !== globalThis) {
      throw new TypeError("Failed to execute 'fetch' on 'Window': Illegal invocation");
    }
    calls.push(String(input));
    return Promise.resolve(jsonResponse({ namespaces: ['default'] }));
  };

  globalThis.fetch = guardedFetch as typeof globalThis.fetch;

  try {
    const client = new ApiClient({ baseUrl: 'https://aion.example' });
    await expect(client.listNamespaces()).resolves.toEqual(['default']);
    expect(calls).toEqual(['https://aion.example/namespaces']);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test('startWorkflow posts the server-shaped body and normalizes the run ids', async () => {
  const calls: Request[] = [];
  const client = new ApiClient({
    baseUrl: 'https://aion.example',
    credentials: { subject: 'operator', namespaces: [namespace] },
    fetchImpl: async (input, init) => {
      calls.push(new Request(input, init));
      return jsonResponse({
        workflow_id: '00000000-0000-0000-0000-0000000000aa',
        run_id: '00000000-0000-0000-0000-0000000000bb',
      });
    },
  });

  const result = await client.startWorkflow(
    { workflowType: 'EmailDigest', input: { to: 'ops' }, routingKey: 'shard-1' },
    { namespace }
  );

  expect(result).toEqual({
    workflowId: '00000000-0000-0000-0000-0000000000aa',
    runId: '00000000-0000-0000-0000-0000000000bb',
  });
  expect(calls[0]?.url).toBe('https://aion.example/workflows/start');
  expect(calls[0]?.headers.get('x-aion-namespaces')).toBe(namespace);
  await expect(calls[0]?.json()).resolves.toEqual({
    namespace,
    workflow_type: 'EmailDigest',
    input: { to: 'ops' },
    routing_key: 'shard-1',
  });
});

test('startWorkflow omits routing_key when not provided', async () => {
  const calls: Request[] = [];
  const client = new ApiClient({
    baseUrl: 'https://aion.example',
    fetchImpl: async (input, init) => {
      calls.push(new Request(input, init));
      return jsonResponse({ workflow_id: 'w', run_id: 'r' });
    },
  });

  await client.startWorkflow({ workflowType: 'T', input: {} }, { namespace });

  await expect(calls[0]?.json()).resolves.toEqual({
    namespace,
    workflow_type: 'T',
    input: {},
  });
});

test('startWorkflow forwards the selected task_queue to the server body', async () => {
  const calls: Request[] = [];
  const client = new ApiClient({
    baseUrl: 'https://aion.example',
    fetchImpl: async (input, init) => {
      calls.push(new Request(input, init));
      return jsonResponse({ workflow_id: 'w', run_id: 'r' });
    },
  });

  await client.startWorkflow({ workflowType: 'T', input: {}, taskQueue: 'gpu' }, { namespace });

  await expect(calls[0]?.json()).resolves.toEqual({
    namespace,
    workflow_type: 'T',
    input: {},
    task_queue: 'gpu',
  });
});

test('startWorkflow omits task_queue when not provided', async () => {
  const calls: Request[] = [];
  const client = new ApiClient({
    baseUrl: 'https://aion.example',
    fetchImpl: async (input, init) => {
      calls.push(new Request(input, init));
      return jsonResponse({ workflow_id: 'w', run_id: 'r' });
    },
  });

  await client.startWorkflow({ workflowType: 'T', input: {} }, { namespace });

  const body = (await calls[0]?.json()) as Record<string, unknown>;
  expect('task_queue' in body).toBe(false);
});

test('startWorkflow throws a typed ApiError on WorkflowTypeNotFound', async () => {
  const client = new ApiClient({
    fetchImpl: async () =>
      jsonResponse(
        { code: 'not_found', message: 'workflow type T is not registered' },
        { status: 404 }
      ),
  });

  try {
    await client.startWorkflow({ workflowType: 'T', input: {} }, { namespace });
    throw new Error('expected startWorkflow to throw');
  } catch (error) {
    expect(error).toBeInstanceOf(ApiError);
    expect((error as ApiError).status).toBe(404);
    expect((error as ApiError).code).toBe('not_found');
  }
});

test('deployPackage sends the raw archive as octet-stream with no baked deploy header', async () => {
  const calls: Request[] = [];
  const client = new ApiClient({
    baseUrl: 'https://aion.example',
    credentials: { subject: 'operator', namespaces: [namespace] },
    fetchImpl: async (input, init) => {
      calls.push(new Request(input, init));
      return jsonResponse({
        workflow_type: 'EmailDigest',
        content_hash: 'blake3:abc',
        deployed_entry_module: 'mod',
        entry_function: 'main',
        freshly_loaded: true,
        route_changed: true,
      });
    },
  });

  const archive = new Uint8Array([1, 2, 3, 4]);
  const result = await client.deployPackage(archive);

  expect(result).toEqual({
    workflowType: 'EmailDigest',
    contentHash: 'blake3:abc',
    deployedEntryModule: 'mod',
    entryFunction: 'main',
    freshlyLoaded: true,
    routeChanged: true,
  });
  expect(calls[0]?.url).toBe('https://aion.example/deploy/packages');
  expect(calls[0]?.method).toBe('POST');
  expect(calls[0]?.headers.get('content-type')).toBe('application/octet-stream');
  // Deploy is authorized server-side (operator mode) or by the bearer's deploy
  // claim (real auth); the console never bakes an x-aion-deploy header.
  expect(calls[0]?.headers.get('x-aion-deploy')).toBeNull();
  expect(calls[0]?.headers.get('x-aion-namespaces')).toBeNull();
  const sent = new Uint8Array(await (calls[0] as Request).arrayBuffer());
  expect(Array.from(sent)).toEqual([1, 2, 3, 4]);
});

test('listVersions normalizes the deploy versions listing', async () => {
  const calls: Request[] = [];
  const client = new ApiClient({
    baseUrl: 'https://aion.example',
    credentials: { subject: 'operator' },
    fetchImpl: async (input, init) => {
      calls.push(new Request(input, init));
      return jsonResponse({
        versions: [
          {
            workflow_type: 'EmailDigest',
            content_hash: 'blake3:abc',
            deployed_entry_module: 'mod',
            entry_function: 'main',
            manifest_version: '1.0.0',
            loaded_at: '2026-06-12T00:00:00Z',
            route_active: true,
          },
        ],
      });
    },
  });

  const versions = await client.listVersions();

  expect(versions).toHaveLength(1);
  expect(versions[0]?.workflowType).toBe('EmailDigest');
  expect(versions[0]?.routeActive).toBe(true);
  expect(calls[0]?.method).toBe('GET');
  // No baked deploy header — authorization is the server's decision.
  expect(calls[0]?.headers.get('x-aion-deploy')).toBeNull();
});

test('listVersions surfaces a 404 (deploy disabled) as a typed ApiError', async () => {
  const client = new ApiClient({
    credentials: { subject: 'operator' },
    fetchImpl: async () => jsonResponse({ message: 'not found' }, { status: 404 }),
  });

  try {
    await client.listVersions();
    throw new Error('expected listVersions to throw');
  } catch (error) {
    expect(error).toBeInstanceOf(ApiError);
    expect((error as ApiError).status).toBe(404);
  }
});

function jsonResponse(body: unknown, init?: ResponseInit): Response {
  return new Response(JSON.stringify(body), {
    headers: { 'content-type': 'application/json' },
    ...init,
  });
}
