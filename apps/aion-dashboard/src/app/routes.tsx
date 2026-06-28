import { type ComponentType, lazy, Suspense } from 'react';
import { createBrowserRouter, type RouteObject, useParams, useSearchParams } from 'react-router';

import { LoadingSkeleton } from '@/components/LoadingSkeleton';
import { FailoverFallback, FailoverView } from '@/features/failover';
import { useNamespace } from '@/features/namespace';
import { WorkflowDetailView } from '@/features/workflow-detail';
import { WorkflowList } from '@/features/workflow-list';
import type { Namespace } from '@/types';

import { AppShell } from './AppShell';
import {
  failoverPath,
  incidentsPath,
  routerBasenameFromBaseUrl,
  searchPath,
  workflowDetailPath,
  workflowListPath,
} from './routePaths';

// Re-export the path contract + href helpers so existing importers of './routes'
// keep working; the canonical, dependency-free source is './routePaths'.
export {
  failoverHref,
  failoverPath,
  incidentsHref,
  incidentsPath,
  routerBasenameFromBaseUrl,
  searchHref,
  searchPath,
  workflowDetailHref,
  workflowDetailPath,
  workflowListHref,
  workflowListPath,
} from './routePaths';

// Lazy view roots for slices that land later (S7 triage, S8 search). Lazy-loading
// isolates each feature module so a build failure in one cannot break unrelated
// routes. The list/detail/failover roots ship today and load eagerly.
const SearchView = lazyNamed(
  () => import('@/features/search'),
  (mod) => mod.SearchView
);
const TriageView = lazyNamed(
  () => import('@/features/triage'),
  (mod) => mod.TriageView
);

// All routes nest under the shell layout route so the header + nav persist
// across navigation and the active route mounts in its <Outlet>.
export const appRoutes: RouteObject[] = [
  {
    element: <AppShell />,
    children: [
      { path: workflowListPath, element: <WorkflowListRoute /> },
      { path: workflowDetailPath, element: <WorkflowDetailRoute /> },
      { path: searchPath, element: <SearchRoute /> },
      { path: incidentsPath, element: <IncidentsRoute /> },
      { path: failoverPath, element: <FailoverRoute /> },
    ],
  },
];

export function createAppRouter(basename = routerBasenameFromBaseUrl()) {
  return createBrowserRouter(appRoutes, { basename });
}

function WorkflowListRoute() {
  const { selectedNamespace } = useNamespace();

  return <WorkflowList namespace={selectedNamespace} />;
}

function WorkflowDetailRoute() {
  const { selectedNamespace } = useNamespace();
  const { id } = useParams<'id'>();

  return <WorkflowDetailView namespace={selectedNamespace} workflowId={id ?? ''} />;
}

function SearchRoute() {
  const { selectedNamespace } = useNamespace();

  return (
    <Suspense fallback={<LoadingSkeleton />}>
      <SearchView namespace={selectedNamespace} />
    </Suspense>
  );
}

function IncidentsRoute() {
  const { selectedNamespace } = useNamespace();

  return (
    <Suspense fallback={<LoadingSkeleton />}>
      <TriageView namespace={selectedNamespace} />
    </Suspense>
  );
}

function FailoverRoute() {
  const { selectedNamespace } = useNamespace();
  const [searchParams] = useSearchParams();

  if (searchParams.get('mode') === 'fallback') {
    return <FailoverFallback namespace={selectedNamespace} />;
  }

  return <FailoverView namespace={selectedNamespace} />;
}

// React.lazy expects a default export; this adapts a named export so feature
// modules keep their named-export convention (CO4) without a default re-export.
function lazyNamed<TMod, TProps extends { namespace: Namespace | null }>(
  loader: () => Promise<TMod>,
  pick: (mod: TMod) => ComponentType<TProps>
) {
  return lazy(async () => ({ default: pick(await loader()) }));
}
