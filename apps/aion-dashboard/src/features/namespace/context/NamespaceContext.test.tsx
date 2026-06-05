import { expect, test } from 'bun:test';

import { applyNamespaceSelection, selectNamespace } from './NamespaceContext';

test('selecting a namespace updates the context value used by the provider', () => {
  let selectedNamespace = 'tenant-a';

  applyNamespaceSelection(
    (namespace) => {
      selectedNamespace = namespace;
    },
    'tenant-b',
    ['tenant-a', 'tenant-b']
  );

  expect(selectedNamespace).toBe('tenant-b');
});

test('selecting a namespace rejects empty or unavailable values', () => {
  expect(() => selectNamespace('', ['tenant-a'])).toThrow('namespace must be selected');
  expect(() => selectNamespace('tenant-b', ['tenant-a'])).toThrow('not available');
});
