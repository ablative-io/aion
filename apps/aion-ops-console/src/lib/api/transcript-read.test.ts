import { expect, test } from 'bun:test';

import type { ActivityEvent } from '@/types';

import { ApiError } from './api-error';
import { TranscriptReadClient } from './transcript-read';

const namespace = 'default';
const workflowId = '00000000-0000-0000-0000-000000000001';

const target = {
  namespace,
  workflowId,
  activityId: 3,
  attempt: 1,
};

function retainedEvent(storeSeq: number): ActivityEvent {
  return {
    workflow_id: workflowId,
    activity_id: 3,
    attempt: 1,
    agent_id: '00000000-0000-0000-0000-0000000000aa',
    agent_role: 'orchestrator',
    emitted_at: '2026-07-01T00:00:00Z',
    worker_seq: 1,
    store_seq: storeSeq,
    ephemeral: false,
    kind: { kind: 'Message', role: { role: 'Assistant' }, text: `seq-${storeSeq}` },
  };
}

function jsonResponse(body: unknown, init?: ResponseInit): Response {
  return new Response(JSON.stringify(body), {
    headers: { 'content-type': 'application/json' },
    ...init,
  });
}

function capturingClient(body: unknown, calls: Request[], status = 200): TranscriptReadClient {
  return new TranscriptReadClient({
    baseUrl: 'https://aion.example',
    credentials: {
      bearerToken: 'token',
      subject: 'operator',
      namespaces: [namespace],
    },
    fetchImpl: async (input, init) => {
      const request = new Request(input, init);
      calls.push(request);
      return jsonResponse(body, { status });
    },
  });
}

test('fetchTranscript posts the stream identity with scoped headers and returns the retained events', async () => {
  const calls: Request[] = [];
  const events = [retainedEvent(0), retainedEvent(1)];
  const client = capturingClient({ events }, calls);

  const result = await client.fetchTranscript(target);

  expect(result).toEqual(events);
  expect(result.map((event) => event.store_seq)).toEqual([0, 1]);
  expect(calls).toHaveLength(1);
  expect(calls[0]?.url).toBe('https://aion.example/workflows/transcript');
  expect(calls[0]?.method).toBe('POST');
  expect(calls[0]?.headers.get('content-type')).toBe('application/json');
  expect(calls[0]?.headers.get('authorization')).toBe('Bearer token');
  expect(calls[0]?.headers.get('x-aion-subject')).toBe('operator');
  expect(calls[0]?.headers.get('x-aion-namespaces')).toBe(namespace);
  // NO from_seq key when unset: the server reads the whole retained transcript.
  await expect(calls[0]?.json()).resolves.toEqual({
    namespace,
    workflow_id: workflowId,
    activity_id: 3,
    attempt: 1,
  });
});

test('fetchTranscript carries from_seq when resuming', async () => {
  const calls: Request[] = [];
  const client = capturingClient({ events: [] }, calls);

  await client.fetchTranscript({ ...target, fromSeq: 3 });

  await expect(calls[0]?.json()).resolves.toEqual({
    namespace,
    workflow_id: workflowId,
    activity_id: 3,
    attempt: 1,
    from_seq: 3,
  });
});

test('fetchTranscript surfaces a malformed envelope as an ApiError', async () => {
  const calls: Request[] = [];
  const client = capturingClient({}, calls);

  await expect(client.fetchTranscript(target)).rejects.toThrow(ApiError);
  await expect(client.fetchTranscript(target)).rejects.toThrow('events');
});

test('listStreams normalizes retained stream heads', async () => {
  const calls: Request[] = [];
  const client = capturingClient({ streams: [{ activity_id: 3, attempt: 0, head: 5 }] }, calls);

  const streams = await client.listStreams(namespace, workflowId);

  expect(streams).toEqual([{ activityId: 3, attempt: 0, head: 5 }]);
  expect(calls[0]?.url).toBe('https://aion.example/workflows/transcripts');
  await expect(calls[0]?.json()).resolves.toEqual({ namespace, workflow_id: workflowId });
});

test('listStreams throws on a malformed stream row (truth-first, never skipped)', async () => {
  const calls: Request[] = [];
  const client = capturingClient({ streams: [{ activity_id: 'three', attempt: 0 }] }, calls);

  await expect(client.listStreams(namespace, workflowId)).rejects.toThrow(ApiError);
});

test('a non-OK transcript read throws the server wire error', async () => {
  const calls: Request[] = [];
  const client = capturingClient(
    { error: { code: 'namespace_denied', message: 'denied' } },
    calls,
    403
  );

  try {
    await client.fetchTranscript(target);
    throw new Error('expected the transcript read to reject');
  } catch (error) {
    if (!(error instanceof ApiError)) {
      throw new Error(`expected an ApiError, got ${String(error)}`);
    }
    expect(error.status).toBe(403);
  }
});
