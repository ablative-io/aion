import { describe, expect, test } from 'bun:test';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';

import type { ApiClient, Capabilities } from '@/lib/api';

import { CapabilitiesProvider, useCapabilities } from '../context/CapabilitiesContext';

function client(capabilities: Capabilities): Pick<ApiClient, 'getCapabilities'> {
  return { getCapabilities: async () => capabilities };
}

function wrap(node: ReactNode, apiClient: Pick<ApiClient, 'getCapabilities'>): string {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

  return renderToStaticMarkup(
    <QueryClientProvider client={queryClient}>
      <CapabilitiesProvider apiClient={apiClient}>{node}</CapabilitiesProvider>
    </QueryClientProvider>
  );
}

function Probe() {
  const { deployGranted, capabilities } = useCapabilities();

  return (
    <div>
      <span data-testid="deploy">{deployGranted ? 'granted' : 'denied'}</span>
      <span data-testid="caps">{capabilities === null ? 'loading' : 'loaded'}</span>
    </div>
  );
}

describe('CapabilitiesProvider', () => {
  test('treats the caller as unprivileged before /whoami resolves', () => {
    // On the synchronous server render the query has not resolved, so the
    // capability is null and deploy is denied — affordances never flash on.
    const markup = wrap(
      <Probe />,
      client({
        subject: 'operator',
        authEnabled: false,
        deployGranted: true,
        allNamespaces: true,
        namespaces: [],
      })
    );

    expect(markup).toContain('denied');
    expect(markup).toContain('loading');
  });

  test('useCapabilities throws outside a provider', () => {
    expect(() => renderToStaticMarkup(<Probe />)).toThrow(
      'useCapabilities must be used within a CapabilitiesProvider'
    );
  });
});
