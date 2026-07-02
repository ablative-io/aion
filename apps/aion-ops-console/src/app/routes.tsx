import { type ComponentType, lazy, Suspense } from 'react';
import { createBrowserRouter, type RouteObject, useParams, useSearchParams } from 'react-router';

import { LoadingSkeleton } from '@/components/LoadingSkeleton';
import { useCapabilities } from '@/features/capabilities';
import { FailoverFallback, FailoverView } from '@/features/failover';
import { NamespaceRegistryPanel, useNamespace } from '@/features/namespace';
import { WorkflowDetailView } from '@/features/workflow-detail';
import { WorkflowList } from '@/features/workflow-list';
import type { Namespace } from '@/types';

import { AppShell } from './AppShell';
import { NotFound } from './NotFound';
import { RouteError } from './RouteError';
import {
  actionsPath,
  failoverPath,
  incidentsPath,
  kitPath,
  namespacesPath,
  routerBasenameFromBaseUrl,
  searchPath,
  workflowDetailPath,
  workflowListPath,
} from './routePaths';

// Re-export the path contract + href helpers so existing importers of './routes'
// keep working; the canonical, dependency-free source is './routePaths'.
export {
  actionsHref,
  actionsPath,
  failoverHref,
  failoverPath,
  incidentsHref,
  incidentsPath,
  kitHref,
  kitPath,
  namespacesHref,
  namespacesPath,
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
const ActionsView = lazyNamed(
  () => import('@/features/actions'),
  (mod) => mod.ActionsView
);
const SearchView = lazyNamed(
  () => import('@/features/search'),
  (mod) => mod.SearchView
);
const TriageView = lazyNamed(
  () => import('@/features/triage'),
  (mod) => mod.TriageView
);
// The kit lookbook takes no namespace, so it adapts its named export directly.
const KitLookbook = lazy(async () => ({
  default: (await import('@/features/kit-lookbook')).KitLookbook,
}));

// All routes nest under the shell layout route so the header + nav persist
// across navigation and the active route mounts in its <Outlet>.
export const appRoutes: RouteObject[] = [
  {
    element: <AppShell />,
    // Branded route error element: a thrown route render, a route-error
    // response, or an unmatched URL renders inside the app chrome via
    // RouteError, never React Router's default developer page.
    errorElement: <RouteError />,
    children: [
      { path: workflowListPath, element: <WorkflowListRoute /> },
      { path: workflowDetailPath, element: <WorkflowDetailRoute /> },
      { path: searchPath, element: <SearchRoute /> },
      { path: actionsPath, element: <ActionsRoute /> },
      { path: incidentsPath, element: <IncidentsRoute /> },
      { path: namespacesPath, element: <NamespaceRegistryPanel /> },
      { path: failoverPath, element: <FailoverRoute /> },
      // Design lookbook (Phase 0 motion kit) — route only, never in the nav.
      { path: kitPath, element: <KitRoute /> },
      // Catch-all: an unknown URL renders the branded 404 inside the shell so
      // the nav persists, instead of falling through to the RR default.
      { path: '*', element: <NotFound /> },
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

function ActionsRoute() {
  const { selectedNamespace } = useNamespace();
  // Deploy affordance is gated on the runtime-discovered capability, never a
  // build-time flag: the panel renders only for a caller the server authorizes.
  const { deployGranted } = useCapabilities();
  // Palette deep-links (`/actions?workflow_type=X`) preload the start form.
  const [searchParams] = useSearchParams();
  const initialWorkflowType = searchParams.get('workflow_type') ?? undefined;

  return (
    <Suspense fallback={<LoadingSkeleton />}>
      <ActionsView
        deployGranted={deployGranted}
        initialWorkflowType={initialWorkflowType}
        namespace={selectedNamespace}
      />
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

function KitRoute() {
  return (
    <Suspense fallback={<LoadingSkeleton />}>
      <KitLookbook />
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
