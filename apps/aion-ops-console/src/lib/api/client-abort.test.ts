import { expect, test } from 'bun:test';

import { ApiClient } from './client';

const namespace = 'default';
const workflowId = '00000000-0000-0000-0000-000000000001';

test('namespaced requests pass the recovery AbortSignal to fetch', async () => {
  const controller = new AbortController();
  let receivedSignal: AbortSignal | null | undefined;
  const client = new ApiClient({
    fetchImpl: async (_input, init) => {
      receivedSignal = init?.signal;
      return Response.json({ history: [] });
    },
  });

  await client.getHistory(workflowId, { namespace, signal: controller.signal });

  expect(receivedSignal).toBe(controller.signal);
  controller.abort();
  expect(receivedSignal?.aborted).toBeTrue();
});
