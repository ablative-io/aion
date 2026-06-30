import { RouterProvider } from 'react-router';

import { RootErrorBoundary } from '@/components/RootErrorBoundary';

import { AppProviders, type AppProvidersProps } from './providers';
import { createAppRouter } from './routes';

export type AppProps = Omit<AppProvidersProps, 'children'> & {
  router?: ReturnType<typeof createAppRouter>;
};

/**
 * App root. Providers wrap the router so the WS manager singleton (providers.tsx)
 * and query client survive route navigation (M3). The shell chrome — header,
 * persistent nav, namespace selector, connection indicator — lives in the
 * layout route ({@link AppShell}) inside the router so nav active-state works.
 * A top-level {@link RootErrorBoundary} backstops the router itself (M2); the
 * shell adds a second boundary around the route outlet so a single route fault
 * surfaces visibly without losing the nav.
 */
function App({ router = createAppRouter(), ...providerProps }: AppProps) {
  return (
    <AppProviders {...providerProps}>
      <RootErrorBoundary>
        <RouterProvider router={router} />
      </RootErrorBoundary>
    </AppProviders>
  );
}

export { App };
