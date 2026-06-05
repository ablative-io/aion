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

function jsonResponse(body: unknown, init?: ResponseInit): Response {
  return new Response(JSON.stringify(body), {
    headers: { 'content-type': 'application/json' },
    ...init,
  });
}
