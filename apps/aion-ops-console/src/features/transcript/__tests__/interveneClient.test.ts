import { expect, test } from 'bun:test';

import { ApiClient, ApiError } from '@/lib/api';

const namespace = 'default';
const workflowId = '00000000-0000-0000-0000-000000000001';

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

test('listAttempts posts the workflow+namespace and normalizes attempts with capabilities', async () => {
  const calls: Request[] = [];
  const client = clientReturning(
    {
      attempts: [
        {
          activity_id: 3,
          attempt: 1,
          capabilities: {
            supported: [{ primitive: 'InjectMessage' }, { primitive: 'Cancel' }],
          },
        },
        // An observability-only attempt: the empty set is first-class.
        { activity_id: 4, attempt: 2, capabilities: { supported: [] } },
      ],
    },
    calls
  );

  const attempts = await client.listAttempts(workflowId, { namespace });
  expect(calls[0]?.url).toBe('https://aion.example/workflows/attempts');
  expect(await calls[0]?.clone().json()).toEqual({ namespace, workflow_id: workflowId });
  expect(attempts).toHaveLength(2);
  expect(attempts[0]?.capabilities.supported).toEqual([
    { primitive: 'InjectMessage' },
    { primitive: 'Cancel' },
  ]);
  expect(attempts[1]?.capabilities.supported).toEqual([]);
});

test('listAttempts drops a row missing addressable target fields, never a phantom control', async () => {
  const client = clientReturning(
    {
      attempts: [
        { activity_id: 3, attempt: 1, capabilities: { supported: [] } },
        // Malformed: no activity_id — must be dropped, not surfaced.
        { attempt: 9, capabilities: { supported: [] } },
      ],
    },
    []
  );

  const attempts = await client.listAttempts(workflowId, { namespace });
  expect(attempts).toHaveLength(1);
  expect(attempts[0]?.activityId).toBe(3);
});

test('intervene surfaces each neutral outcome VERBATIM (applied/gated/stale)', async () => {
  for (const outcome of [
    { outcome: 'Applied' },
    { outcome: 'CapabilityNotSupported', primitive: { primitive: 'PauseResume' } },
    { outcome: 'StaleTarget', detail: 'attempt superseded' },
  ]) {
    const client = clientReturning({ outcome }, []);
    const result = await client.intervene(
      {
        workflowId,
        activityId: 3,
        attempt: 1,
        kind: { kind: 'Cancel', reason: 'stop' },
      },
      { namespace }
    );
    // The ack is returned as-is; a gated/stale outcome is NOT reinterpreted.
    expect(result).toEqual(outcome as never);
  }
});

test('intervene posts only the target + primitive (issued_by/at are server-stamped)', async () => {
  const calls: Request[] = [];
  const client = clientReturning({ outcome: { outcome: 'Applied' } }, calls);
  await client.intervene(
    {
      workflowId,
      activityId: 3,
      attempt: 2,
      kind: { kind: 'InjectMessage', text: 'steer', priority: { priority: 'Interrupt' } },
    },
    { namespace }
  );
  const body = (await calls[0]?.clone().json()) as Record<string, unknown>;
  expect(body).toEqual({
    namespace,
    workflow_id: workflowId,
    activity_id: 3,
    attempt: 2,
    kind: { kind: 'InjectMessage', text: 'steer', priority: { priority: 'Interrupt' } },
  });
  expect(body.issued_by).toBeUndefined();
  expect(body.issued_at).toBeUndefined();
});

test('a malformed intervene response is a typed error, never a silent success', async () => {
  const client = clientReturning({ not_outcome: true }, []);
  await expect(
    client.intervene(
      { workflowId, activityId: 3, attempt: 1, kind: { kind: 'Cancel', reason: 'x' } },
      { namespace }
    )
  ).rejects.toBeInstanceOf(ApiError);
});

test('a non-2xx intervene response propagates a typed ApiError', async () => {
  const client = new ApiClient({
    baseUrl: 'https://aion.example',
    credentials: { namespaces: [namespace] },
    fetchImpl: async () => jsonResponse({ error: { code: 'namespace_denied' } }, { status: 403 }),
  });
  await expect(
    client.intervene(
      { workflowId, activityId: 3, attempt: 1, kind: { kind: 'Cancel', reason: 'x' } },
      { namespace }
    )
  ).rejects.toBeInstanceOf(ApiError);
});
