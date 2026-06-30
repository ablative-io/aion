import { expect, test } from 'bun:test';
import { QueryClient } from '@tanstack/react-query';
import { renderToStaticMarkup } from 'react-dom/server';
import { createMemoryRouter } from 'react-router';

import { App } from './App';
import { connectWebSocketLifecycle } from './providers';
import { failoverPath, incidentsPath, searchPath, workflowListPath } from './routePaths';
import { appRoutes } from './routes';

const workflow = {
  workflow_id: 'workflow-1',
  workflow_type: 'EmailDigest',
  status: 'Running' as const,
  started_at: '2026-06-05T20:00:00Z',
  ended_at: null,
  parent: null,
};

const workflowStartedEvent = {
  type: 'WorkflowStarted' as const,
  data: {
    envelope: {
      seq: 1,
      recorded_at: '2026-06-05T20:00:00Z',
      workflow_id: workflow.workflow_id,
    },
    workflow_type: workflow.workflow_type,
    input: { content_type: 'Json' as const, bytes: [] },
  },
};

// Every Phase-1 route must mount inside the shell without crashing. Lazy routes
// (search, incidents) render their Suspense fallback under SSR; the assertion is
// that the shell chrome is present and nothing throws.
const SHELL_ROUTES = [
  workflowListPath,
  '/workflows/workflow-1',
  searchPath,
  incidentsPath,
  failoverPath,
];

test('mounts every Phase-1 route inside the persistent shell without crashing', () => {
  for (const path of SHELL_ROUTES) {
    const markup = renderApp(path);

    expect(markup).toContain('Aion Ops Console');
    // Persistent nav is present on every route.
    expect(markup).toContain('>Workflows<');
    expect(markup).toContain('>Search<');
    expect(markup).toContain('>Incidents<');
    expect(markup).toContain('>Failover<');
    expect(markup).toContain('Namespace');
  }
});

test('marks exactly the active route in the nav with aria-current', () => {
  const onList = renderApp(workflowListPath);
  expect(countAriaCurrent(onList)).toBe(1);
  expect(activeNavLabel(onList)).toBe('Workflows');

  const onSearch = renderApp(searchPath);
  expect(countAriaCurrent(onSearch)).toBe(1);
  expect(activeNavLabel(onSearch)).toBe('Search');

  const onIncidents = renderApp(incidentsPath);
  expect(activeNavLabel(onIncidents)).toBe('Incidents');

  const onFailover = renderApp(failoverPath);
  expect(activeNavLabel(onFailover)).toBe('Failover');
});

test('keeps the Workflows tab active on a workflow detail route', () => {
  const onDetail = renderApp('/workflows/workflow-1');

  expect(activeNavLabel(onDetail)).toBe('Workflows');
});

// M3: the shared WS manager connects once and is torn down only on full unmount.
test('WS lifecycle connects once and closes once across its lifetime', () => {
  let connects = 0;
  let closes = 0;
  const manager = {
    connect: () => {
      connects += 1;
    },
    close: () => {
      closes += 1;
    },
  };

  const teardown = connectWebSocketLifecycle(manager);
  expect(connects).toBe(1);
  expect(closes).toBe(0);

  teardown();
  expect(connects).toBe(1);
  expect(closes).toBe(1);
});

// M3 (structural): the WS provider lives above the router, so the route table —
// which is what changes on navigation — must not embed any WS lifecycle. If it
// did, route churn could remount and tear down the socket.
test('route table does not own the WS lifecycle (provider sits above the router)', () => {
  const serialized = JSON.stringify(appRoutes, replaceElementsWithNames);

  expect(serialized).not.toContain('WebSocketConnection');
  expect(serialized).not.toContain('AppProviders');
});

function renderApp(initialPath: string) {
  const router = createMemoryRouter(appRoutes, { initialEntries: [initialPath] });
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  seedList(queryClient);
  seedHistory(queryClient);

  return renderToStaticMarkup(
    <App
      apiClient={{
        listNamespaces: async () => ['default'],
        getCapabilities: async () => ({
          subject: 'operator',
          authEnabled: false,
          deployGranted: true,
          allNamespaces: true,
          namespaces: [],
        }),
      }}
      initialNamespace="default"
      queryClient={queryClient}
      router={router}
      websocketManager={{ close: () => undefined, connect: () => undefined }}
    />
  );
}

function countAriaCurrent(markup: string): number {
  return markup.split('aria-current="page"').length - 1;
}

function activeNavLabel(markup: string): string | null {
  // The active link carries aria-current="page"; capture its text content.
  const match = markup.match(/aria-current="page"[^>]*>([^<]+)</);
  return match?.[1] ?? null;
}

function replaceElementsWithNames(_key: string, value: unknown): unknown {
  if (value !== null && typeof value === 'object' && 'type' in value) {
    const elementType: unknown = value.type;
    if (typeof elementType === 'function') {
      return { element: elementType.name };
    }
  }
  return value;
}

const listRequest = {
  filter: {
    parent: null,
    started_after: null,
    started_before: null,
    status: null,
    workflow_type: null,
  },
  page: { cursor: undefined, limit: 50 },
};

function seedList(queryClient: QueryClient) {
  queryClient.setQueryData(['workflows', 'default', listRequest.filter, listRequest.page], {
    hasMore: false,
    items: [workflow],
    nextCursor: null,
  });
}

function seedHistory(queryClient: QueryClient) {
  queryClient.setQueryData(
    ['workflow-history', 'default', workflow.workflow_id],
    [workflowStartedEvent]
  );
}
