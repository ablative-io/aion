import { useQuery } from '@tanstack/react-query';

import { useNamespace } from '@/features/namespace';
import type { EventSearchResult, WorkflowPage } from '@/lib/api';
import { ApiClient, type EventSearchQuery, type WorkflowPageRequest } from '@/lib/api';
import type { Namespace } from '@/types';

const defaultApiClient = new ApiClient();

/**
 * The form's raw string state. Distinct from {@link EventSearchQuery}: this is
 * what the inputs hold (always strings, possibly blank); {@link buildEventSearchQuery}
 * collapses it to the pruned wire query and reports whether any field is set.
 */
export type EventSearchFormState = {
  eventType: string;
  workflowType: string;
  activityType: string;
  errorText: string;
  recordedAfter: string;
  recordedBefore: string;
};

export const EMPTY_EVENT_SEARCH_FORM: EventSearchFormState = {
  eventType: '',
  workflowType: '',
  activityType: '',
  errorText: '',
  recordedAfter: '',
  recordedBefore: '',
};

/**
 * Collapse the form state into the wire query, dropping blank fields. The fields
 * are AND-combined server-side; an all-blank form is reported as `isEmpty` so the
 * caller can refuse to run a "match everything" query rather than silently doing so.
 */
export function buildEventSearchQuery(state: EventSearchFormState): {
  query: EventSearchQuery;
  isEmpty: boolean;
} {
  const query: EventSearchQuery = {};

  assignTrimmed(query, 'eventType', state.eventType);
  assignTrimmed(query, 'workflowType', state.workflowType);
  assignTrimmed(query, 'activityType', state.activityType);
  assignTrimmed(query, 'errorText', state.errorText);
  assignTrimmed(query, 'recordedAfter', toIsoBound(state.recordedAfter));
  assignTrimmed(query, 'recordedBefore', toIsoBound(state.recordedBefore));

  return { query, isEmpty: Object.keys(query).length === 0 };
}

export function eventSearchQueryKey(
  namespace: Namespace | null,
  query: EventSearchQuery,
  page: WorkflowPageRequest = {}
) {
  return ['event-search', namespace, query, page] as const;
}

export type EventSearchOptions = {
  apiClient?: Pick<ApiClient, 'searchEvents'>;
  /** Pruned wire query; empty queries are not run (see `enabled`). */
  query: EventSearchQuery;
  page?: WorkflowPageRequest;
  /** Caller gate (e.g. the user pressed Search). Defaults to true. */
  enabled?: boolean;
};

/**
 * Run a field-aware event search against the AW endpoint. The query is disabled
 * when no namespace is selected or the query is empty, so the form never issues a
 * "match all" request. A server-side failure (including a 404 if the endpoint is
 * not yet pinned) propagates as `query.error` for the view to surface — never
 * swallowed, never faked.
 */
export function useEventSearch({
  apiClient = defaultApiClient,
  query,
  page = {},
  enabled = true,
}: EventSearchOptions) {
  const { selectedNamespace } = useNamespace();
  const hasNamespace = selectedNamespace !== null && selectedNamespace.trim().length > 0;
  const hasQuery = Object.keys(query).length > 0;

  return useQuery<WorkflowPage<EventSearchResult>>({
    enabled: enabled && hasNamespace && hasQuery,
    queryKey: eventSearchQueryKey(selectedNamespace, query, page),
    queryFn: () =>
      apiClient.searchEvents(query, page, {
        namespace: requireSearchNamespace(selectedNamespace),
      }),
  });
}

function requireSearchNamespace(namespace: Namespace | null): Namespace {
  if (namespace === null || namespace.trim().length === 0) {
    throw new Error('A namespace must be selected before searching events.');
  }

  return namespace;
}

function assignTrimmed<K extends keyof EventSearchQuery>(
  target: EventSearchQuery,
  key: K,
  value: string | undefined
): void {
  if (value === undefined) {
    return;
  }

  const trimmed = value.trim();

  if (trimmed.length > 0) {
    target[key] = trimmed;
  }
}

/**
 * `datetime-local` inputs yield a local "YYYY-MM-DDTHH:mm" with no zone. Promote
 * it to an ISO-8601 instant so the bound is unambiguous on the wire; blank stays blank.
 */
function toIsoBound(value: string): string {
  const trimmed = value.trim();

  if (trimmed.length === 0) {
    return '';
  }

  const parsed = new Date(trimmed);

  return Number.isNaN(parsed.getTime()) ? trimmed : parsed.toISOString();
}
