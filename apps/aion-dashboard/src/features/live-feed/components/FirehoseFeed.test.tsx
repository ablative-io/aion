import { expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import type { Event, Namespace } from '@/types';
import { FirehoseFeedContent } from './FirehoseFeed';

const namespace = 'default' as Namespace;

test('FirehoseFeedContent renders explicit empty and disconnected states', () => {
  const markup = renderToStaticMarkup(
    <FirehoseFeedContent events={[]} namespace={namespace} status="disconnected" />
  );

  expect(markup).toContain('No live events yet');
  expect(markup).toContain('Live socket is disconnected');
});

test('FirehoseFeedContent renders streamed events in newest-first order', () => {
  const markup = renderToStaticMarkup(
    <FirehoseFeedContent
      events={[workflowStarted(1), workflowCompleted(2)]}
      namespace={namespace}
      status="connected"
    />
  );

  expect(markup).toContain('Live firehose');
  expect(markup.indexOf('seq 2')).toBeLessThan(markup.indexOf('seq 1'));
  expect(markup).toContain('WorkflowCompleted');
  expect(markup).toContain('workflow workflow-1');
});

function workflowStarted(seq: number): Event {
  return {
    type: 'WorkflowStarted',
    data: {
      envelope: { seq, recorded_at: '2026-06-05T20:00:00Z', workflow_id: 'workflow-1' },
      input: { content_type: 'Json', bytes: [123, 125] },
      workflow_type: 'checkout',
    },
  };
}

function workflowCompleted(seq: number): Event {
  return {
    type: 'WorkflowCompleted',
    data: {
      envelope: { seq, recorded_at: '2026-06-05T20:01:00Z', workflow_id: 'workflow-1' },
      result: { content_type: 'Json', bytes: [123, 125] },
    },
  };
}
