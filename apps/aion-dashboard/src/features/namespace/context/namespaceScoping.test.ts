import { expect, test } from 'bun:test';

import { namespaceSubscriptionFilter, subscribeToNamespaceFilter } from '@/features/live-feed';
import { workflowHistoryQueryKey, workflowHistoryRequestOptions } from '@/features/workflow-detail';
import { workflowListQueryKey, workflowQueryRequestOptions } from '@/features/workflow-list';
import type { EventSubscriptionManager } from '@/lib/api/websocket';
import type { WorkflowFilter } from '@/types';

const filter: WorkflowFilter = {
  parent: null,
  started_after: null,
  started_before: null,
  status: null,
  workflow_type: null,
};

test('workflow list and history query keys include the selected namespace', () => {
  expect(workflowListQueryKey('tenant-a', filter, { limit: 25 })).toEqual([
    'workflows',
    'tenant-a',
    filter,
    { limit: 25 },
  ]);
  expect(workflowHistoryQueryKey('tenant-a', 'workflow-1')).toEqual([
    'workflow-history',
    'tenant-a',
    'workflow-1',
  ]);
  expect(workflowQueryRequestOptions('tenant-a')).toEqual({ namespace: 'tenant-a' });
  expect(workflowHistoryRequestOptions('tenant-a')).toEqual({ namespace: 'tenant-a' });
});

test('query request helpers reject missing namespaces instead of issuing unscoped calls', () => {
  expect(() => workflowQueryRequestOptions(null)).toThrow('namespace must be selected');
  expect(() => workflowHistoryRequestOptions(null)).toThrow('namespace must be selected');
});

test('switching namespace produces re-scoped query keys and subscription filters', () => {
  const firstListKey = workflowListQueryKey('tenant-a', filter, { cursor: 'next' });
  const nextListKey = workflowListQueryKey('tenant-b', filter, { cursor: 'next' });
  const firstSubscription = namespaceSubscriptionFilter('tenant-a', { mode: 'firehose' });
  const nextSubscription = namespaceSubscriptionFilter('tenant-b', { mode: 'firehose' });

  expect(firstListKey).not.toEqual(nextListKey);
  expect(nextListKey[1]).toBe('tenant-b');
  expect(firstSubscription).toEqual({ mode: 'firehose', namespace: 'tenant-a' });
  expect(nextSubscription).toEqual({ mode: 'firehose', namespace: 'tenant-b' });
});

test('subscription scoping preserves filters while carrying namespace and replay cursor', () => {
  expect(namespaceSubscriptionFilter('tenant-b', { mode: 'filtered', filter }, 42)).toEqual({
    afterSeq: 42,
    filter,
    mode: 'filtered',
    namespace: 'tenant-b',
  });
});

test('switching namespace re-establishes the subscription with the new namespace', () => {
  const filters: unknown[] = [];
  const manager: EventSubscriptionManager = {
    subscribe(subscriptionFilter) {
      filters.push(subscriptionFilter);
      return () => undefined;
    },
  };

  subscribeToNamespaceFilter(manager, 'tenant-a', { mode: 'firehose' }, () => undefined);
  subscribeToNamespaceFilter(manager, 'tenant-b', { mode: 'firehose' }, () => undefined);

  expect(filters).toEqual([
    { mode: 'firehose', namespace: 'tenant-a' },
    { mode: 'firehose', namespace: 'tenant-b' },
  ]);
  expect(filters).not.toContainEqual({ mode: 'firehose' });
});
