import { expect, test } from 'bun:test';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { renderToStaticMarkup } from 'react-dom/server';

import { NamespaceProvider } from '@/features/namespace';
import type { Event } from '@/types';

import {
  fullPayloadEventQueryOptions,
  type PayloadEventClient,
  PayloadView,
} from '../components/PayloadView';

const workflowId = '00000000-0000-0000-0000-000000000001';
const elidedEvent: Event = workflowStarted({ __elided: true, size_bytes: 4096 });
const fullEvent: Event = workflowStarted([123, 10, 32, 32, 34, 86, 34, 58, 32, 49, 10, 125]);

test('elided payload renders a compact size-only load affordance without decoding', () => {
  const queryClient = new QueryClient();
  const markup = renderToStaticMarkup(
    <QueryClientProvider client={queryClient}>
      <NamespaceProvider
        apiClient={{ listNamespaces: async () => ['default'] }}
        initialNamespace="default"
      >
        <PayloadView event={elidedEvent} payload={elidedEvent.data.input} />
      </NamespaceProvider>
    </QueryClientProvider>
  );

  expect(markup).toContain('4 KB — load');
  expect(markup).not.toContain('size_bytes');
});

test('on-demand full event fetch is cached by workflow id and sequence', async () => {
  let calls = 0;
  const client: PayloadEventClient = {
    getEvent: async (requestedWorkflowId, seq, options) => {
      calls += 1;
      expect(requestedWorkflowId).toBe(workflowId);
      expect(seq).toBe(7);
      expect(options.namespace).toBe('default');
      return fullEvent;
    },
  };
  const queryClient = new QueryClient();
  const options = fullPayloadEventQueryOptions(client, elidedEvent, 'default');

  await expect(queryClient.fetchQuery(options)).resolves.toEqual(fullEvent);
  await expect(queryClient.fetchQuery(options)).resolves.toEqual(fullEvent);

  expect(calls).toBe(1);
  expect(options.queryKey).toEqual(['workflow-event', workflowId, 7]);
});

function workflowStarted(bytes: EventBytes): Extract<Event, { type: 'WorkflowStarted' }> {
  return {
    type: 'WorkflowStarted',
    data: {
      envelope: {
        seq: 7,
        recorded_at: '2026-07-01T00:00:00Z',
        workflow_id: workflowId,
      },
      workflow_type: 'payload-test',
      input: { content_type: 'Json', bytes },
      run_id: '00000000-0000-0000-0000-0000000000aa',
      parent_run_id: null,
      package_version: '1.0.0',
    },
  };
}

type EventBytes = Extract<Event, { type: 'WorkflowStarted' }>['data']['input']['bytes'];
