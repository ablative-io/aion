import { expect, test } from 'bun:test';
import { QueryClient } from '@tanstack/react-query';
import { renderToStaticMarkup } from 'react-dom/server';
import { createMemoryRouter } from 'react-router';

import { App } from './App';
import { appRoutes, routerBasenameFromBaseUrl, workflowDetailHref } from './routes';

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

test('renders the provider-backed list route in the app shell', () => {
  const markup = renderApp('/');

  expect(markup).toContain('Aion Dashboard');
  expect(markup).toContain('Namespace');
  expect(markup).toContain('Event stream');
  expect(markup).toContain('workflow-1');
  expect(markup).toContain('href="/workflows/workflow-1"');
});

test('renders the detail route and can create a list router again for back navigation', () => {
  const detailMarkup = renderApp('/workflows/workflow-1');
  const listMarkup = renderApp('/');

  expect(detailMarkup).toContain('Workflow workflow-1');
  expect(detailMarkup).toContain('Workflow started: EmailDigest');
  expect(listMarkup).toContain('workflow-1');
});

test('normalizes Vite base paths for the router basename', () => {
  expect(routerBasenameFromBaseUrl('/aion/')).toBe('/aion');
  expect(routerBasenameFromBaseUrl('/')).toBe('/');
  expect(workflowDetailHref('workflow-1')).toBe('/workflows/workflow-1');
});

function renderApp(initialPath: string) {
  const router = createMemoryRouter(appRoutes, { initialEntries: [initialPath] });
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
    },
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
