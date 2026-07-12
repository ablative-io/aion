import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import { RevisionDriftNotice } from './ShipRunPanel';

describe('run-mode deployed revision binding', () => {
  test('shows the explicit drift state and diff affordance when the buffer moved', () => {
    const html = renderToStaticMarkup(
      <RevisionDriftNotice
        deployed="workflow deployed"
        editor="workflow edited"
        onToggle={() => undefined}
        showDiff={false}
      />
    );
    expect(html).toContain('editor has drifted from the running revision');
    expect(html).toContain('View diff');
  });

  test('shows no drift state when the buffer equals the deployed revision', () => {
    const html = renderToStaticMarkup(
      <RevisionDriftNotice
        deployed="workflow same"
        editor="workflow same"
        onToggle={() => undefined}
        showDiff={false}
      />
    );
    expect(html).toBe('');
  });

  test('renders both immutable deployed text and editor text after one-click diff', () => {
    const html = renderToStaticMarkup(
      <RevisionDriftNotice
        deployed="deployed text"
        editor="editor text"
        onToggle={() => undefined}
        showDiff
      />
    );
    expect(html).toContain('Deployed revision');
    expect(html).toContain('deployed text');
    expect(html).toContain('Editor buffer');
    expect(html).toContain('editor text');
  });
});
