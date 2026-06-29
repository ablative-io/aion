import { useMemo, useState } from 'react';
import { Link } from 'react-router';

import { workflowDetailHref } from '@/app/routePaths';
import { EmptyState } from '@/components/EmptyState';
import { ErrorState } from '@/components/ErrorState';
import { LoadingSkeleton } from '@/components/LoadingSkeleton';
import type { ApiClient, EventSearchResult } from '@/lib/api';
import type { Namespace } from '@/types';

import {
  buildEventSearchQuery,
  EMPTY_EVENT_SEARCH_FORM,
  type EventSearchFormState,
  useEventSearch,
} from '../hooks/useEventSearch';
import { SearchForm } from './SearchForm';

export type SearchViewProps = {
  namespace: Namespace | null;
  client?: ApiClient | undefined;
};

/**
 * Event-level search (plan slice S8, §4.5). A field-aware form posts to the AW
 * event-search endpoint; results deep-link into the swimlane at the matching
 * event (`/workflows/:id?seq=N`). No namespace, no query, or a server failure
 * each surface to visible state — results are never fabricated.
 */
export function SearchView({ namespace, client }: SearchViewProps) {
  const [formState, setFormState] = useState<EventSearchFormState>(EMPTY_EVENT_SEARCH_FORM);
  const [submitted, setSubmitted] = useState<EventSearchFormState | null>(null);

  const { isEmpty } = useMemo(() => buildEventSearchQuery(formState), [formState]);
  const submittedQuery = useMemo(
    () => (submitted === null ? { query: {}, isEmpty: true } : buildEventSearchQuery(submitted)),
    [submitted]
  );

  const search = useEventSearch({
    apiClient: client,
    enabled: submitted !== null && !submittedQuery.isEmpty,
    query: submittedQuery.query,
  });

  function handleSubmit() {
    if (isEmpty) {
      return;
    }

    setSubmitted(formState);
  }

  function handleClear() {
    setFormState(EMPTY_EVENT_SEARCH_FORM);
    setSubmitted(null);
  }

  return (
    <section aria-label="Event search" className="space-y-4 py-4">
      <header>
        <h1 className="font-medium text-[var(--text-primary)] text-xl">Event search</h1>
        <p className="text-[var(--text-muted)] text-sm">
          Search recorded workflow events by field; results open the swimlane at the matching event.
        </p>
      </header>

      {namespace === null ? (
        <EmptyState
          description="Select a namespace to scope the event search."
          title="No namespace selected"
        />
      ) : (
        <>
          <SearchForm
            isEmpty={isEmpty}
            isLoading={search.isFetching}
            onChange={setFormState}
            onClear={handleClear}
            onSubmit={handleSubmit}
            value={formState}
          />
          <SearchResults
            error={search.error}
            hasSearched={submitted !== null}
            isError={search.isError}
            isLoading={search.isLoading && search.fetchStatus !== 'idle'}
            onRetry={() => {
              void search.refetch();
            }}
            results={search.data?.items ?? []}
          />
        </>
      )}
    </section>
  );
}

type SearchResultsProps = {
  isError: boolean;
  isLoading: boolean;
  error: unknown;
  hasSearched: boolean;
  onRetry: () => void;
  results: EventSearchResult[];
};

function SearchResults({
  isError,
  isLoading,
  error,
  hasSearched,
  onRetry,
  results,
}: SearchResultsProps) {
  if (!hasSearched) {
    return (
      <EmptyState
        description="Set one or more fields above and run a search."
        title="No search yet"
      />
    );
  }

  if (isError) {
    return <ErrorState error={error} onRetry={onRetry} title="Event search failed" />;
  }

  if (isLoading) {
    return <LoadingSkeleton />;
  }

  if (results.length === 0) {
    return (
      <EmptyState
        description="No recorded events match this query in the selected namespace."
        title="No matching events"
      />
    );
  }

  return (
    <ul className="space-y-2">
      {results.map((result) => (
        <SearchResultRow key={`${result.workflowId}:${result.seq}`} result={result} />
      ))}
    </ul>
  );
}

function SearchResultRow({ result }: { result: EventSearchResult }) {
  const { event, workflowId, seq } = result;

  return (
    <li>
      <Link
        className="flex items-center justify-between gap-4 rounded-md border border-[var(--border-default)] px-4 py-3 hover:border-[var(--text-muted)]"
        to={workflowDetailHref(workflowId, seq)}
      >
        <span className="flex flex-col">
          <span className="font-medium text-[var(--text-primary)] text-sm">{event.type}</span>
          <span className="text-[var(--text-muted)] text-xs">{workflowId}</span>
        </span>
        <span className="flex flex-col items-end">
          <span className="font-mono text-[var(--text-muted)] text-xs">seq {seq}</span>
          <span className="text-[var(--text-muted)] text-xs">
            {formatRecordedAt(event.data.envelope.recorded_at)}
          </span>
        </span>
      </Link>
    </li>
  );
}

function formatRecordedAt(value: string): string {
  const parsed = new Date(value);

  if (Number.isNaN(parsed.getTime())) {
    return value;
  }

  return parsed.toISOString().replace('T', ' ').slice(0, 19);
}
