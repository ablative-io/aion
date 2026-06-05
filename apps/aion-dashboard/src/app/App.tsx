import { RouterProvider } from 'react-router';

import { ConnectionIndicator } from '@/features/live-feed';
import { NamespaceSelector } from '@/features/namespace';

import { AppProviders, type AppProvidersProps } from './providers';
import { createAppRouter } from './routes';

export type AppProps = Omit<AppProvidersProps, 'children'> & {
  router?: ReturnType<typeof createAppRouter>;
};

function App({ router = createAppRouter(), ...providerProps }: AppProps) {
  return (
    <AppProviders {...providerProps}>
      <main className="min-h-screen bg-background text-foreground">
        <div className="mx-auto flex min-h-screen max-w-7xl flex-col px-6 py-6">
          <header className="border-border/70 flex flex-col gap-4 border-b pb-6 md:flex-row md:items-center md:justify-between">
            <div className="space-y-1">
              <p className="font-medium text-muted-foreground text-sm uppercase tracking-[0.2em]">
                Aion Dashboard
              </p>
              <h1 className="font-semibold text-3xl tracking-tight">Workflow operations</h1>
            </div>
            <div className="flex flex-col gap-3 sm:flex-row sm:items-end">
              <NamespaceSelector />
              <ConnectionIndicator />
            </div>
          </header>
          <section className="flex-1 py-6">
            <RouterProvider router={router} />
          </section>
        </div>
      </main>
    </AppProviders>
  );
}

export { App };
