import { expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import type { Event, Payload } from '@/types';

import { EventTimeline } from '../components/EventTimeline';
import { PayloadView, stringifyPayload } from '../components/PayloadView';
import { projectTimeline } from '../lib/timeline';

const payload: Payload = { content_type: 'Json', bytes: [123, 125] };
const workflowId = 'workflow-1';

test('PayloadView renders collapsed payload summary by default', () => {
  const markup = renderToStaticMarkup(<PayloadView payload={{ value: 'hidden-detail' }} />);

  expect(markup).toContain('aria-expanded="false"');
  expect(markup).toContain('hidden-detail');
  expect(markup).not.toContain('<pre');
});

test('PayloadView formats generated JSON payload bytes as decoded data', () => {
  expect(stringifyPayload(payload)).toBe('{}');
});

test('static timeline renders child workflow links and sequence metadata', () => {
  const markup = renderToStaticMarkup(
    <EventTimeline events={[childStarted(2), workflowStarted(1)]} />
  );

  expect(markup).toContain('seq 1');
  expect(markup).toContain('/workflows/child-1');
});

test('static timeline sorts projected entries passed directly by sequence', () => {
  const markup = renderToStaticMarkup(
    <EventTimeline entries={projectTimeline([childStarted(2), workflowStarted(1)]).toReversed()} />
  );

  expect(markup.indexOf('seq 1')).toBeLessThan(markup.indexOf('seq 2'));
});

function envelope(seq: number) {
  return { seq, recorded_at: `2026-06-05T20:00:0${seq}Z`, workflow_id: workflowId };
}

function workflowStarted(seq: number): Event {
  return {
    type: 'WorkflowStarted',
    data: {
      envelope: envelope(seq),
      workflow_type: 'checkout',
      input: payload,
      run_id: '00000000-0000-0000-0000-0000000000a1',
      parent_run_id: null,
      package_version: '1.0.0',
    },
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
      package_version: '1.0.0',
    },
  };
}
