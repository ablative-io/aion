import { createBrowserRouter, generatePath, useParams, type RouteObject } from 'react-router';

import { useNamespace } from '@/features/namespace';
import { WorkflowDetail } from '@/features/workflow-detail';
import { WorkflowList } from '@/features/workflow-list';

export const workflowListPath = '/';
export const workflowDetailPath = '/workflows/:id';

export const appRoutes: RouteObject[] = [
  {
    path: workflowListPath,
    element: <WorkflowListRoute />,
  },
  {
    path: workflowDetailPath,
    element: <WorkflowDetailRoute />,
  },
];

export function createAppRouter(basename = routerBasenameFromBaseUrl()) {
  return createBrowserRouter(appRoutes, { basename });
}

export function workflowDetailHref(workflowId: string) {
  return generatePath(workflowDetailPath, { id: workflowId });
}

export function routerBasenameFromBaseUrl(baseUrl = import.meta.env.BASE_URL) {
  if (baseUrl === '' || baseUrl === '/') {
    return '/';
  }

  const withoutTrailingSlash = baseUrl.endsWith('/') ? baseUrl.slice(0, -1) : baseUrl;
  return withoutTrailingSlash.length === 0 ? '/' : withoutTrailingSlash;
}

function WorkflowListRoute() {
  const { selectedNamespace } = useNamespace();

  return <WorkflowList namespace={selectedNamespace} />;
}

function WorkflowDetailRoute() {
  const { selectedNamespace } = useNamespace();
  const { id } = useParams<'id'>();

  return <WorkflowDetail namespace={selectedNamespace} workflowId={id ?? ''} />;
}
