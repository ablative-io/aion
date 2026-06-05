import { expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import { Button } from '@/components/ui';

import { App } from './App';

test('renders the scaffold shell with a shadcn primitive', () => {
  const markup = renderToStaticMarkup(<App />);
  const primitiveMarkup = renderToStaticMarkup(<Button type="button">Ready</Button>);

  expect(markup).toContain('Operational UI scaffold');
  expect(markup).toContain('data-connection-status="disconnected"');
  expect(primitiveMarkup).toContain('data-slot="button"');
});
