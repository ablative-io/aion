import { isRouteErrorResponse, Link, useRouteError } from 'react-router';

import { ErrorState } from '@/components/ErrorState';

import { workflowListPath } from './routePaths';

/**
 * Branded route-level error element wired to the root route's `errorElement`.
 *
 * React Router renders its own unstyled "Hey developer" page when a route
 * throws or a navigation 404s and no `errorElement` is present. Mounting this
 * inside the app shell instead keeps the dashboard chrome and surfaces the
 * fault on the visible {@link ErrorState} — a thrown loader/render error, a
 * route-error response, or an unknown URL all land here, never the framework
 * default.
 */
export function RouteError() {
  const error = useRouteError();
  const { title, message } = describeRouteError(error);

  return (
    <main className="min-h-screen bg-background text-foreground">
      <div className="mx-auto flex min-h-screen max-w-7xl flex-col px-6 py-6">
        <header className="border-border/70 border-b pb-6">
          <p className="font-medium text-muted-foreground text-sm uppercase tracking-[0.2em]">
            Aion Dashboard
          </p>
        </header>
        <section className="flex-1 py-6">
          <ErrorState error={error} message={message} title={title} />
          <Link
            className="mt-4 inline-block text-[var(--accent-cyan)] text-sm underline-offset-4 hover:underline"
            to={workflowListPath}
          >
            Back to workflows
          </Link>
        </section>
      </div>
    </main>
  );
}

function describeRouteError(error: unknown): { title: string; message: string } {
  if (isRouteErrorResponse(error)) {
    if (error.status === 404) {
      return {
        title: 'Page not found',
        message: 'This URL does not map to any dashboard view.',
      };
    }

    return {
      title: `Request failed (${error.status})`,
      message: error.statusText.length > 0 ? error.statusText : 'The route returned an error.',
    };
  }

  return {
    title: 'The dashboard hit an unexpected error',
    message: error instanceof Error ? error.message : 'An unknown error occurred.',
  };
}
