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
