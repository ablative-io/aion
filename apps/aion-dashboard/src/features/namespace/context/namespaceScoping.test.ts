import { expect, test } from 'bun:test';

import { requireSelectedNamespace } from '@/features/namespace';
import { workflowHistoryQueryKey, workflowHistoryRequestOptions } from '@/features/workflow-detail';
import { workflowListQueryKey, workflowQueryRequestOptions } from '@/features/workflow-list';
import type { FirehoseEventSubscriptionFilter } from '@/lib/api';
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
  expect(() => workflowQueryRequestOptions('')).toThrow('namespace must be selected');
  expect(() => workflowHistoryRequestOptions(null)).toThrow('namespace must be selected');
  expect(() => workflowHistoryRequestOptions('')).toThrow('namespace must be selected');
});

test('switching namespace produces re-scoped query keys and subscription filters', () => {
  const firstListKey = workflowListQueryKey('tenant-a', filter, { cursor: 'next' });
  const nextListKey = workflowListQueryKey('tenant-b', filter, { cursor: 'next' });
  const firstSubscription = firehoseFilter('tenant-a');
  const nextSubscription = firehoseFilter('tenant-b');

  expect(firstListKey).not.toEqual(nextListKey);
  expect(nextListKey[1]).toBe('tenant-b');
  expect(firstSubscription).toEqual({ kind: 'firehose', namespace: 'tenant-a' });
  expect(nextSubscription).toEqual({ kind: 'firehose', namespace: 'tenant-b' });
});

test('subscription scoping rejects an empty namespace before subscribing', () => {
  expect(() => firehoseFilter('')).toThrow('namespace must be selected');
});

function firehoseFilter(namespace: string): FirehoseEventSubscriptionFilter {
  return { kind: 'firehose', namespace: requireSelectedNamespace(namespace, 'subscribing to events') };
}
