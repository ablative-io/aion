import { describe, expect, test } from 'bun:test';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { renderToStaticMarkup } from 'react-dom/server';
import { MemoryRouter } from 'react-router';

import { NamespaceProvider } from '@/features/namespace';
import type { ApiClient, EventSearchResult, WorkflowPage } from '@/lib/api';
import { KeybindingsProvider } from '@/lib/keybindings';
import type { Event, Namespace } from '@/types';

import { SearchView } from '../components/SearchView';

const NAMESPACE = 'default' as Namespace;

function makeEvent(seq: number): Event {
  return {
    type: 'ActivityFailed',
    data: {
      envelope: {
        seq,
        recorded_at: '2026-06-05T20:00:00.000Z',
        workflow_id: 'workflow-1',
      },
      activity_id: 1,
      attempt: 1,
      error: { kind: 'Terminal', message: 'boom', details: null },
    },
  };
}

const resultPage: WorkflowPage<EventSearchResult> = {
  items: [{ event: makeEvent(7), workflowId: 'workflow-1', seq: 7 }],
  nextCursor: null,
  hasMore: false,
};

const namespaceClient = {
  listNamespaces: async () => [NAMESPACE],
} as unknown as Pick<ApiClient, 'listNamespaces'>;

function render(
  searchClient?: Pick<ApiClient, 'searchEvents'>,
  namespace: Namespace | null = NAMESPACE
) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });

  return renderToStaticMarkup(
    <QueryClientProvider client={queryClient}>
      {/* The view registers its focus-search handler with the central registry. */}
      <KeybindingsProvider>
        <NamespaceProvider apiClient={namespaceClient} initialNamespace={NAMESPACE}>
          <MemoryRouter>
            <SearchView client={searchClient as ApiClient | undefined} namespace={namespace} />
          </MemoryRouter>
        </NamespaceProvider>
      </KeybindingsProvider>
    </QueryClientProvider>
  );
}

describe('SearchView', () => {
  test('renders the no-namespace state when none is selected', () => {
    const markup = render(undefined, null);

    expect(markup).toContain('No namespace selected');
  });

  test('shows the form and the no-search-yet state before a query runs', () => {
    const markup = render();

    expect(markup).toContain('Event type');
    expect(markup).toContain('No search yet');
  });

  test('an empty query disables submit and shows the hint', () => {
    const markup = render();

    expect(markup).toContain('an empty query is not run');
    expect(markup).toContain('disabled=""');
  });
});

describe('search result rendering (unit shape)', () => {
  test('a result carries a swimlane deep-link with its seq', () => {
    const result = resultPage.items[0];

    expect(result).toBeDefined();
    expect(result?.seq).toBe(7);
    expect(result?.event.type).toBe('ActivityFailed');
    expect(result?.event.data.envelope.recorded_at).toBe('2026-06-05T20:00:00.000Z');
  });
});
