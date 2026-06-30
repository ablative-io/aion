import { Link, useLocation } from 'react-router';

import { EmptyState } from '@/components/EmptyState';

import { workflowListPath } from './routePaths';

/**
 * Branded 404 rendered by the `{ path: '*' }` catch-all route. It nests inside
 * the app shell so the header + nav persist, and surfaces the unknown path so
 * an operator who mistyped a URL sees the dashboard, never React Router's
 * default developer page.
 */
export function NotFound() {
  const { pathname } = useLocation();

  return (
    <div className="space-y-4">
      <EmptyState
        message={`No dashboard view is mapped to "${pathname}".`}
        title="Page not found"
      />
      <Link
        className="inline-block text-[var(--accent-cyan)] text-sm underline-offset-4 hover:underline"
        to={workflowListPath}
      >
        Back to workflows
      </Link>
    </div>
  );
}
