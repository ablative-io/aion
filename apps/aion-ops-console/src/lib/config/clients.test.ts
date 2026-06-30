import { expect, test } from 'bun:test';

import type { WorkflowFilter } from '@/types';

import { createConfiguredApiClient } from './clients';
import { parseOpsConsoleConfig } from './env';

const filter: WorkflowFilter = {
  workflow_type: null,
  status: null,
  started_after: null,
  started_before: null,
  parent: null,
};

function captureFetch() {
  const calls: Request[] = [];
  const fetchImpl = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    // Same-origin config yields relative URLs (e.g. `/namespaces`). A browser
    // resolves those against the document origin; `new Request` in the test
    // runtime has no document, so resolve against a stand-in origin to mirror
    // browser behaviour (absolute-URL configs are unaffected).
    const resolved =
      typeof input === 'string' && input.startsWith('/')
        ? `http://same-origin.test${input}`
        : input;
    calls.push(new Request(resolved, init));
    return new Response(JSON.stringify({ items: [], next_cursor: null, has_more: false }), {
      headers: { 'content-type': 'application/json' },
    });
  };

  return { calls, fetchImpl };
}

test('configured client sends x-aion-namespaces scoped to the selected namespace', async () => {
  const { calls, fetchImpl } = captureFetch();
  const config = parseOpsConsoleConfig({
    VITE_AION_API_BASE: 'http://127.0.0.1:8090',
    VITE_AION_NAMESPACES: 'default,ops',
    VITE_AION_SUBJECT: 'operator',
  });

  const client = createConfiguredApiClient({ config, namespace: 'default', fetchImpl });
  await client.queryWorkflows(filter, {}, { namespace: 'default' });

  expect(calls).toHaveLength(1);
  expect(calls[0]?.url).toBe('http://127.0.0.1:8090/workflows/list');
  // The header that was missing in the live rehearsal (caused namespace_denied).
  expect(calls[0]?.headers.get('x-aion-namespaces')).toBe('default');
  expect(calls[0]?.headers.get('x-aion-subject')).toBe('operator');
});

test('configured client sends the full grant on listNamespaces (no selected namespace)', async () => {
  const { calls, fetchImpl } = captureFetch();
  const config = parseOpsConsoleConfig({ VITE_AION_NAMESPACES: 'default,ops' });

  const client = createConfiguredApiClient({ config, fetchImpl });
  await client.listNamespaces();

  // No VITE_AION_API_BASE => same origin => relative URL.
  expect(calls[0]?.url).toMatch(/\/namespaces$/);
  expect(calls[0]?.headers.get('x-aion-namespaces')).toBe('default,ops');
});

test('configured client sends a bearer token when configured', async () => {
  const { calls, fetchImpl } = captureFetch();
  const config = parseOpsConsoleConfig({
    VITE_AION_NAMESPACES: 'default',
    VITE_AION_BEARER_TOKEN: 'jwt-token',
  });

  const client = createConfiguredApiClient({ config, namespace: 'default', fetchImpl });
  await client.queryWorkflows(filter, {}, { namespace: 'default' });

  expect(calls[0]?.headers.get('authorization')).toBe('Bearer jwt-token');
});

test('baseUrl override repoints the client at a survivor node (failover own-read)', async () => {
  const { calls, fetchImpl } = captureFetch();
  const config = parseOpsConsoleConfig({
    VITE_AION_API_BASE: 'http://127.0.0.1:8090',
    VITE_AION_NAMESPACES: 'default',
  });

  const client = createConfiguredApiClient({
    config,
    namespace: 'default',
    baseUrl: 'http://127.0.0.1:8092',
    fetchImpl,
  });
  await client.queryWorkflows(filter, {}, { namespace: 'default' });

  expect(calls[0]?.url).toBe('http://127.0.0.1:8092/workflows/list');
  expect(calls[0]?.headers.get('x-aion-namespaces')).toBe('default');
});

test('an unconfigured client sends no x-aion-namespaces (no fabricated credentials)', async () => {
  const { calls, fetchImpl } = captureFetch();
  const config = parseOpsConsoleConfig({});

  const client = createConfiguredApiClient({ config, fetchImpl });
  await client.queryWorkflows(filter, {}, { namespace: 'default' });

  expect(calls[0]?.headers.get('x-aion-namespaces')).toBeNull();
});
