import { Link, Outlet, useLocation } from 'react-router';

import { AppNav, type AppNavItem, type AppNavLinkProps } from '@/components/AppNav';
import { RootErrorBoundary } from '@/components/RootErrorBoundary';
import { ConnectionIndicator } from '@/features/live-feed';
import { NamespaceSelector } from '@/features/namespace';

import {
  failoverPath,
  incidentsPath,
  searchPath,
  workflowDetailPath,
  workflowListPath,
} from './routePaths';

// Primary nav. `activePrefix` keeps "Workflows" lit while on a detail route.
const NAV_ITEMS: readonly AppNavItem[] = [
  { href: workflowListPath, label: 'Workflows', activePrefix: workflowListPath },
  { href: searchPath, label: 'Search', activePrefix: searchPath },
  { href: incidentsPath, label: 'Incidents', activePrefix: incidentsPath },
  { href: failoverPath, label: 'Failover', activePrefix: failoverPath },
];

const WORKFLOW_DETAIL_PREFIX = workflowDetailPath.replace('/:id', '');

/**
 * Persistent app shell rendered by the layout route: header chrome + primary
 * nav + namespace selector + connection indicator, with the active route
 * mounted in the {@link Outlet}. The error boundary wraps only the outlet so a
 * route render fault surfaces visibly (no white-screen) while the nav stays
 * usable. Hand-plane: text-link nav, no chrome.
 */
export function AppShell() {
  const { pathname } = useLocation();
  // A detail route ("/workflows/:id") should keep the Workflows tab active.
  const navPath = pathname.startsWith(`${WORKFLOW_DETAIL_PREFIX}/`) ? workflowListPath : pathname;

  return (
    <main className="min-h-screen bg-background text-foreground">
      <div className="mx-auto flex min-h-screen max-w-7xl flex-col px-6 py-6">
        <header className="border-border/70 flex flex-col gap-4 border-b pb-6 md:flex-row md:items-center md:justify-between">
          <div className="space-y-3">
            <p className="font-medium text-muted-foreground text-sm uppercase tracking-[0.2em]">
              Aion Dashboard
            </p>
            <AppNav currentPath={navPath} items={NAV_ITEMS} renderLink={renderNavLink} />
          </div>
          <div className="flex flex-col gap-3 sm:flex-row sm:items-end">
            <NamespaceSelector />
            <ConnectionIndicator />
          </div>
        </header>
        <section className="flex-1 py-6">
          <RootErrorBoundary>
            <Outlet />
          </RootErrorBoundary>
        </section>
      </div>
    </main>
  );
}

function renderNavLink(item: AppNavItem, props: AppNavLinkProps) {
  return (
    <Link aria-current={props['aria-current']} className={props.className} to={item.href}>
      {item.label}
    </Link>
  );
}
