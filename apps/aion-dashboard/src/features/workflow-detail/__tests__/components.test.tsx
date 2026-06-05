import { expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import type { Event, Payload } from '@/types';

import { EventTimeline } from '../components/EventTimeline';
import { PayloadView } from '../components/PayloadView';
import { WorkflowDetailContent } from '../components/WorkflowDetail';

const payload: Payload = { content_type: 'Json', bytes: [123, 125] };
const workflowId = 'workflow-1';

test('PayloadView renders collapsed payload summary by default', () => {
  const markup = renderToStaticMarkup(<PayloadView payload={{ value: 'hidden-detail' }} />);

  expect(markup).toContain('aria-expanded="false"');
  expect(markup).toContain('hidden-detail');
  expect(markup).not.toContain('<pre');
});

test('WorkflowDetailContent renders loading, empty, and error states', () => {
  const baseProps = { namespace: 'default', workflowId };
  const loading = renderToStaticMarkup(
    <WorkflowDetailContent
      {...baseProps}
      error={null}
      history={[]}
      isError={false}
      isLoading={true}
    />
  );
  const empty = renderToStaticMarkup(
    <WorkflowDetailContent
      {...baseProps}
      error={null}
      history={[]}
      isError={false}
      isLoading={false}
    />
  );
  const error = renderToStaticMarkup(
    <WorkflowDetailContent
      {...baseProps}
      error={new Error('history failed')}
      history={[]}
      isError={true}
      isLoading={false}
    />
  );

  expect(loading).toContain('Loading timeline');
  expect(empty).toContain('This workflow has no events yet');
  expect(error).toContain('history failed');
});

test('WorkflowDetailContent renders empty namespace state without an unscoped query', () => {
  const markup = renderToStaticMarkup(
    <WorkflowDetailContent
      error={null}
      history={[]}
      isError={false}
      isLoading={false}
      namespace={null}
      workflowId={workflowId}
    />
  );

  expect(markup).toContain('No namespace selected');
});

test('static timeline renders child workflow links and sequence metadata', () => {
  const markup = renderToStaticMarkup(<EventTimeline events={[childStarted(2), workflowStarted(1)]} />);

  expect(markup).toContain('seq 1');
  expect(markup).toContain('/workflows/child-1');
});

function envelope(seq: number) {
  return { seq, recorded_at: `2026-06-05T20:00:0${seq}Z`, workflow_id: workflowId };
}

function workflowStarted(seq: number): Event {
  return {
    type: 'WorkflowStarted',
    data: { envelope: envelope(seq), workflow_type: 'checkout', input: payload },
  };
}

function childStarted(seq: number): Event {
  return {
    type: 'ChildWorkflowStarted',
    data: {
      envelope: envelope(seq),
      child_workflow_id: 'child-1',
      workflow_type: 'receipt',
      input: payload,
    },
  };
}
