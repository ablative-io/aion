import { expect, test } from 'bun:test';

import type { Event } from '@/types';

import { ApiClient, ApiError } from './client';

const namespace = 'default';
const workflowId = '00000000-0000-0000-0000-000000000001';
const event: Event = workflowStarted([123, 125]);

test('getHistoryWindow posts fixed window fields and decodes elision metadata', async () => {
  const calls: Request[] = [];
  const elidedEvent = workflowStarted({ __elided: true, size_bytes: 4096 });
  const client = capturingClient(calls, {
    events: [elidedEvent],
    next_from_seq: 502,
    head_seq: 900,
  });

  const window = await client.getHistoryWindow(
    workflowId,
    { fromSeq: 2, limit: 500, payloadLimitBytes: 2048 },
    { namespace }
  );

  expect(window).toEqual({ events: [elidedEvent], nextFromSeq: 502, headSeq: 900 });
  expect(calls[0]?.url).toBe('https://aion.example/workflows/history');
  await expect(calls[0]?.json()).resolves.toEqual({
    namespace,
    workflow_id: workflowId,
    from_seq: 2,
    limit: 500,
    payload_limit_bytes: 2048,
  });
});

test('getHistoryWindow omits optional fields so server defaults remain authoritative', async () => {
  const calls: Request[] = [];
  const client = capturingClient(calls, { events: [], next_from_seq: null, head_seq: 0 });

  await client.getHistoryWindow(workflowId, {}, { namespace });

  await expect(calls[0]?.json()).resolves.toEqual({ namespace, workflow_id: workflowId });
});

test('getEvent posts a sequence and unwraps the full event response', async () => {
  const calls: Request[] = [];
  const client = capturingClient(calls, { event });

  await expect(client.getEvent(workflowId, 1, { namespace })).resolves.toEqual(event);
  expect(calls[0]?.url).toBe('https://aion.example/workflows/event');
  await expect(calls[0]?.json()).resolves.toEqual({ namespace, workflow_id: workflowId, seq: 1 });
});

test('history windows reject malformed cursor metadata instead of hiding contract drift', async () => {
  const client = new ApiClient({
    fetchImpl: async () => jsonResponse({ events: [], next_from_seq: 'later', head_seq: 1 }),
  });

  await expect(client.getHistoryWindow(workflowId, {}, { namespace })).rejects.toBeInstanceOf(
    ApiError
  );
});

function capturingClient(calls: Request[], responseBody: unknown): ApiClient {
  return new ApiClient({
    baseUrl: 'https://aion.example',
    fetchImpl: async (input, init) => {
      calls.push(new Request(input, init));
      return jsonResponse(responseBody);
    },
  });
}

function workflowStarted(bytes: EventBytes): Extract<Event, { type: 'WorkflowStarted' }> {
  return {
    type: 'WorkflowStarted',
    data: {
      envelope: {
        seq: 1,
        recorded_at: '2026-07-01T00:00:00Z',
        workflow_id: workflowId,
      },
      workflow_type: 'client-test',
      input: { content_type: 'Json', bytes },
      run_id: '00000000-0000-0000-0000-0000000000aa',
      parent_run_id: null,
      package_version: '1.0.0',
    },
  };
}

type EventBytes = Extract<Event, { type: 'WorkflowStarted' }>['data']['input']['bytes'];

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    headers: { 'content-type': 'application/json' },
  });
}
