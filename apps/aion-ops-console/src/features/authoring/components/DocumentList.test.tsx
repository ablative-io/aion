import { expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import { DocumentList } from './DocumentList';

test('document list renders the empty state', () => {
  const html = renderToStaticMarkup(
    <DocumentList documents={[]} onSelect={() => undefined} selectedPath={null} />
  );
  expect(html).toContain('No AWL documents');
});

test('document list renders the exact unconfigured workspace state', () => {
  const html = renderToStaticMarkup(
    <DocumentList documents={[]} onSelect={() => undefined} selectedPath={null} unconfigured />
  );
  expect(html).toContain('authoring workspace not configured');
  expect(html).not.toContain('No AWL documents');
});

test('document rows expose compact deployment state and selected semantics', () => {
  const html = renderToStaticMarkup(
    <DocumentList
      documents={[
        { name: 'checkout.awl', path: 'workflows/checkout.awl' },
        { name: 'refund.awl', path: 'workflows/refund.awl' },
      ]}
      onSelect={() => undefined}
      selectedPath="workflows/checkout.awl"
    />
  );

  expect(html).toContain('checkout.awl');
  expect(html).toContain('workflows/checkout.awl');
  expect(html).toContain('aria-current="true"');
  expect(html.match(/deploys green/g)?.length).toBe(2);
  expect(html.match(/data-status="healthy"/g)?.length).toBe(2);
});
