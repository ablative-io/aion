import { expect, test } from 'bun:test';

import { buildClusterConfig, defaultClusterNamespace } from './clusterConfig';

test('buildClusterConfig uses the explicit namespace when provided', () => {
  const config = buildClusterConfig({ namespace: 'default', host: '127.0.0.1' });

  expect(config.namespace).toBe('default');
});

test('buildClusterConfig falls back to the injected fallbackNamespace, not a hardcoded demo', () => {
  const config = buildClusterConfig({ fallbackNamespace: 'default' });

  // The live rehearsal gap: the demo cluster uses `default`, not `demo`.
  expect(config.namespace).toBe('default');
  expect(config.namespace).not.toBe('demo');
});

test('defaultClusterNamespace resolves to a configured/default namespace, never demo', () => {
  // With no VITE_AION_NAMESPACES configured in the test env, the fallback is the
  // demo-cluster namespace `default` — never the old hardcoded `demo`.
  expect(defaultClusterNamespace()).not.toBe('demo');
});

test('buildClusterConfig seeds the node topology and owner index', () => {
  const config = buildClusterConfig({
    namespace: 'default',
    host: 'localhost',
    search: '?nodes=2',
  });

  expect(config.nodes).toHaveLength(2);
  expect(config.nodes[0]?.baseUrl).toBe('http://localhost:8090');
  expect(config.nodes[1]?.baseUrl).toBe('http://localhost:8091');
  expect(config.ownerIndex).toBe(0);
});
