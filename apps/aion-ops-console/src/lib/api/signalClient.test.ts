import { expect, test } from 'bun:test';

import { ApiClient, ApiError } from '@/lib/api';

const namespace = 'default';
const workflowId = '00000000-0000-0000-0000-000000000001';
const runId = '00000000-0000-0000-0000-0000000000a1';

function jsonResponse(body: unknown, init?: ResponseInit): Response {
  return new Response(JSON.stringify(body), {
    headers: { 'content-type': 'application/json' },
    ...init,
  });
}

function clientReturning(body: unknown, calls: Request[]): ApiClient {
  return new ApiClient({
    baseUrl: 'https://aion.example',
    credentials: { namespaces: [namespace] },
    fetchImpl: async (input, init) => {
      calls.push(new Request(input, init));
      return jsonResponse(body);
    },
  });
}

test('sendSignal posts the server SignalWorkflowRequest shape with a plain-JSON payload', async () => {
  const calls: Request[] = [];
  const client = clientReturning({}, calls);

  await client.sendSignal(workflowId, 'assistant_continue', { namespace }, { message: 'go on' });

  expect(calls[0]?.url).toBe('https://aion.example/workflows/signal');
  expect(calls[0]?.method).toBe('POST');
  // No run_id is sent when omitted — the server resolves the latest run.
  expect(await calls[0]?.clone().json()).toEqual({
    namespace,
    workflow_id: workflowId,
    signal_name: 'assistant_continue',
    payload: { message: 'go on' },
  });
});

test('sendSignal omits the payload key entirely for a payload-less signal', async () => {
  const calls: Request[] = [];
  const client = clientReturning({}, calls);

  await client.sendSignal(workflowId, 'assistant_end', { namespace });

  expect(await calls[0]?.clone().json()).toEqual({
    namespace,
    workflow_id: workflowId,
    signal_name: 'assistant_end',
  });
});

test('sendSignal forwards an explicit run_id when targeting a specific run', async () => {
  const calls: Request[] = [];
  const client = clientReturning({}, calls);

  await client.sendSignal(workflowId, 'poke', { namespace }, { value: 1 }, runId);

  expect(await calls[0]?.clone().json()).toEqual({
    namespace,
    workflow_id: workflowId,
    signal_name: 'poke',
    payload: { value: 1 },
    run_id: runId,
  });
});

test('a non-2xx signal response propagates a typed ApiError (e.g. not_found)', async () => {
  const client = new ApiClient({
    baseUrl: 'https://aion.example',
    credentials: { namespaces: [namespace] },
    fetchImpl: async () => jsonResponse({ error: { code: 'not_found' } }, { status: 404 }),
  });
  await expect(
    client.sendSignal(workflowId, 'assistant_continue', { namespace }, { message: 'hi' })
  ).rejects.toBeInstanceOf(ApiError);
});
