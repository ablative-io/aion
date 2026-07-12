import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import { WorkerScaffold } from './WorkerScaffold';

describe('WorkerScaffold', () => {
  test('wires the per-worker runtime picker and generation affordance', () => {
    const html = renderToStaticMarkup(<WorkerScaffold source="workflow demo" worker="jobs" />);

    expect(html).toContain('Runtime for jobs');
    expect(html).toContain('value="shell" selected=""');
    expect(html).toContain('value="rust"');
    expect(html).toContain('Generate worker');
  });
});
