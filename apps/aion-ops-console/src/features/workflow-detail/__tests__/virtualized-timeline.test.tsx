import { expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import type { Event } from '@/types';

import { EventTimeline } from '../components/EventTimeline';

test('list timeline renders only the virtual window for a large event history', () => {
  const events = Array.from({ length: 2000 }, (_, seq) => workflowStarted(seq));

  const markup = renderToStaticMarkup(<EventTimeline events={events} />);
  const renderedRows = markup.match(/>seq \d+</g) ?? [];

  expect(renderedRows.length).toBeGreaterThan(0);
  expect(renderedRows.length).toBeLessThan(100);
  expect(markup).toContain('overflow-y-auto');
});

function workflowStarted(seq: number): Event {
  return {
    type: 'WorkflowStarted',
    data: {
      envelope: {
        seq,
        recorded_at: '2026-07-01T00:00:00Z',
        workflow_id: '00000000-0000-0000-0000-000000000001',
      },
      workflow_type: `virtual-${seq}`,
      input: { content_type: 'Json', bytes: [123, 125] },
      run_id: `00000000-0000-0000-0000-${String(seq).padStart(12, '0')}`,
      parent_run_id: null,
      package_version: '1.0.0',
    },
  };
}
