import { expect, test } from 'bun:test';

import { applyNamespaceSelection } from './NamespaceContext';

test('selecting a namespace updates the context value used by the provider', () => {
  let selectedNamespace = 'tenant-a';

  applyNamespaceSelection((namespace) => {
    selectedNamespace = namespace;
  }, 'tenant-b');

  expect(selectedNamespace).toBe('tenant-b');
});
