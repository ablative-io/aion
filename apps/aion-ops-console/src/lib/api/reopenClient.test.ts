import { expect, test } from 'bun:test';

import { ApiClient, ApiError } from '@/lib/api';

const namespace = 'default';
const workflowId = '00000000-0000-0000-0000-000000000001';
const reopenedRunId = '00000000-0000-0000-0000-0000000000a1';

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

test('reopen posts the workflow+namespace and normalizes the reopened run + status', async () => {
  const calls: Request[] = [];
  const client = clientReturning({ run_id: reopenedRunId, status: 'Running' }, calls);

  const result = await client.reopen(workflowId, { namespace });

  expect(calls[0]?.url).toBe('https://aion.example/workflows/reopen');
  expect(calls[0]?.method).toBe('POST');
  // No run_id is sent when omitted — the server resolves the latest run.
  expect(await calls[0]?.clone().json()).toEqual({ namespace, workflow_id: workflowId });
  expect(result).toEqual({ runId: reopenedRunId, status: 'Running' });
});

test('reopen forwards an explicit run_id when targeting a specific run', async () => {
  const calls: Request[] = [];
  const client = clientReturning({ run_id: reopenedRunId, status: 'Running' }, calls);

  await client.reopen(workflowId, { namespace }, reopenedRunId);

  expect(await calls[0]?.clone().json()).toEqual({
    namespace,
    workflow_id: workflowId,
    run_id: reopenedRunId,
  });
});

test('a malformed reopen response is a typed error, never a silent success', async () => {
  // Missing run_id.
  const missingRun = clientReturning({ status: 'Running' }, []);
  await expect(missingRun.reopen(workflowId, { namespace })).rejects.toBeInstanceOf(ApiError);

  // Unknown status — never coerced to a phantom WorkflowStatus.
  const badStatus = clientReturning({ run_id: reopenedRunId, status: 'Bogus' }, []);
  await expect(badStatus.reopen(workflowId, { namespace })).rejects.toBeInstanceOf(ApiError);
});

test('a non-2xx reopen response propagates a typed ApiError (e.g. invalid_state)', async () => {
  const client = new ApiClient({
    baseUrl: 'https://aion.example',
    credentials: { namespaces: [namespace] },
    fetchImpl: async () => jsonResponse({ error: { code: 'invalid_state' } }, { status: 409 }),
  });
  await expect(client.reopen(workflowId, { namespace })).rejects.toBeInstanceOf(ApiError);
});
