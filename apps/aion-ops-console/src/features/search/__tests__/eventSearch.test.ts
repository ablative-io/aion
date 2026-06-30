import { describe, expect, test } from 'bun:test';

import { workflowDetailHref } from '@/app/routePaths';

import {
  buildEventSearchQuery,
  EMPTY_EVENT_SEARCH_FORM,
  type EventSearchFormState,
} from '../hooks/useEventSearch';

describe('buildEventSearchQuery', () => {
  test('an all-blank form is reported empty and produces no wire fields', () => {
    const { query, isEmpty } = buildEventSearchQuery(EMPTY_EVENT_SEARCH_FORM);

    expect(isEmpty).toBe(true);
    expect(Object.keys(query)).toHaveLength(0);
  });

  test('blank and whitespace-only fields are pruned; set fields survive', () => {
    const state: EventSearchFormState = {
      ...EMPTY_EVENT_SEARCH_FORM,
      eventType: 'ActivityFailed',
      workflowType: '   ',
      errorText: '  timeout  ',
    };

    const { query, isEmpty } = buildEventSearchQuery(state);

    expect(isEmpty).toBe(false);
    expect(query.eventType).toBe('ActivityFailed');
    expect(query.errorText).toBe('timeout');
    expect('workflowType' in query).toBe(false);
  });

  test('datetime-local bounds are promoted to ISO-8601 instants', () => {
    const state: EventSearchFormState = {
      ...EMPTY_EVENT_SEARCH_FORM,
      recordedAfter: '2026-06-01T00:00',
    };

    const { query } = buildEventSearchQuery(state);

    expect(query.recordedAfter).toBeDefined();
    expect(query.recordedAfter).toBe(new Date('2026-06-01T00:00').toISOString());
  });

  test('a single set field makes the query non-empty', () => {
    const { isEmpty } = buildEventSearchQuery({
      ...EMPTY_EVENT_SEARCH_FORM,
      activityType: 'SendEmail',
    });

    expect(isEmpty).toBe(false);
  });
});

describe('result deep-link', () => {
  test('href targets the workflow detail with the matching seq', () => {
    expect(workflowDetailHref('workflow-7', 42)).toBe('/workflows/workflow-7?seq=42');
  });

  test('href without a seq is the bare detail path', () => {
    expect(workflowDetailHref('workflow-7')).toBe('/workflows/workflow-7');
  });
});
