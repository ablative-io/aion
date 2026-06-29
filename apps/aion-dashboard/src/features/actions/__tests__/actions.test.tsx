import { describe, expect, test } from 'bun:test';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { MemoryRouter } from 'react-router';

import { NamespaceProvider } from '@/features/namespace';
import { type ApiClient, ApiError, type LoadPackageResult, type WorkflowVersion } from '@/lib/api';
import type { Namespace } from '@/types';

import { ActionsView } from '../components/ActionsView';
import { DeployOutcome, VersionsBody } from '../components/DeployPackagePanel';
import { StartOutcome, StartWorkflowForm } from '../components/StartWorkflowForm';
import { parseJsonInput } from '../lib/jsonInput';

const NAMESPACE = 'default' as Namespace;

const namespaceClient = {
  listNamespaces: async () => [NAMESPACE],
} as unknown as Pick<ApiClient, 'listNamespaces'>;

function wrap(node: ReactNode): string {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

  return renderToStaticMarkup(
    <QueryClientProvider client={queryClient}>
      <NamespaceProvider apiClient={namespaceClient} initialNamespace={NAMESPACE}>
        <MemoryRouter>{node}</MemoryRouter>
      </NamespaceProvider>
    </QueryClientProvider>
  );
}

describe('parseJsonInput', () => {
  test('blank input is the empty object (engine requires a payload)', () => {
    expect(parseJsonInput('   ')).toEqual({ ok: true, value: {} });
  });

  test('a JSON object parses', () => {
    expect(parseJsonInput('{"to": "ops"}')).toEqual({ ok: true, value: { to: 'ops' } });
  });

  test('a non-object (array) is rejected with a visible message', () => {
    const result = parseJsonInput('[1,2]');
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error).toContain('JSON object');
    }
  });

  test('malformed JSON is rejected', () => {
    const result = parseJsonInput('{not json');
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error).toContain('Invalid JSON');
    }
  });
});

describe('StartWorkflowForm', () => {
  test('renders the no-namespace state when none is selected', () => {
    const markup = wrap(<StartWorkflowForm namespace={null} />);
    expect(markup).toContain('No namespace selected');
  });

  test('renders the form fields when a namespace is selected', () => {
    const markup = wrap(<StartWorkflowForm namespace={NAMESPACE} />);
    expect(markup).toContain('Workflow type');
    expect(markup).toContain('Input (JSON)');
    expect(markup).toContain('Start workflow');
  });
});

describe('StartOutcome', () => {
  test('shows confirmed run ids only after the server returns them', () => {
    const markup = wrap(
      <StartOutcome
        error={null}
        isError={false}
        namespace={NAMESPACE}
        result={{ workflowId: 'wf-1', runId: 'run-1' }}
      />
    );

    expect(markup).toContain('Workflow started');
    expect(markup).toContain('wf-1');
    expect(markup).toContain('run-1');
    expect(markup).toContain('/workflows/wf-1');
  });

  test('renders nothing before a confirmed start (no premature success)', () => {
    const markup = wrap(
      <StartOutcome error={null} isError={false} namespace={NAMESPACE} result={null} />
    );

    expect(markup).not.toContain('Workflow started');
  });

  test('surfaces a server error', () => {
    const markup = wrap(
      <StartOutcome
        error={new ApiError(404, 'workflow type T is not registered', 'not_found')}
        isError
        namespace={NAMESPACE}
        result={null}
      />
    );

    expect(markup).toContain('Start failed');
    expect(markup).toContain('not registered');
  });
});

describe('DeployOutcome', () => {
  const result: LoadPackageResult = {
    workflowType: 'EmailDigest',
    contentHash: 'blake3:abc',
    deployedEntryModule: 'mod',
    entryFunction: 'main',
    freshlyLoaded: true,
    routeChanged: true,
  };

  test('shows the deployed content hash on a confirmed upload', () => {
    const markup = wrap(<DeployOutcome error={null} isError={false} result={result} />);
    expect(markup).toContain('Package deployed');
    expect(markup).toContain('blake3:abc');
  });

  test('reports an idempotent re-upload distinctly', () => {
    const markup = wrap(
      <DeployOutcome error={null} isError={false} result={{ ...result, freshlyLoaded: false }} />
    );
    expect(markup).toContain('Already resident');
  });

  test('renders a deploy_denied message for a 403', () => {
    const markup = wrap(
      <DeployOutcome
        error={new ApiError(403, 'deploy denied', 'deploy_denied')}
        isError
        result={null}
      />
    );
    expect(markup).toContain('Deploy failed');
    expect(markup).toContain('deployment-wide deploy grant');
  });
});

describe('VersionsBody', () => {
  const version: WorkflowVersion = {
    workflowType: 'EmailDigest',
    contentHash: 'blake3:abc',
    deployedEntryModule: 'mod',
    entryFunction: 'main',
    manifestVersion: '1.0.0',
    loadedAt: '2026-06-12T00:00:00Z',
    routeActive: true,
  };

  test('renders the explicit deploy-disabled state on a 404', () => {
    const markup = wrap(
      <VersionsBody
        error={new ApiError(404, 'not found')}
        isError
        isLoading={false}
        onRetry={() => undefined}
        versions={[]}
      />
    );

    expect(markup).toContain('Deploy is disabled');
    expect(markup).toContain('[deploy] enabled=true');
  });

  test('renders loaded versions', () => {
    const markup = wrap(
      <VersionsBody
        error={null}
        isError={false}
        isLoading={false}
        onRetry={() => undefined}
        versions={[version]}
      />
    );

    expect(markup).toContain('EmailDigest');
    expect(markup).toContain('blake3:abc');
    expect(markup).toContain('route-active');
  });

  test('renders the empty state with no versions', () => {
    const markup = wrap(
      <VersionsBody
        error={null}
        isError={false}
        isLoading={false}
        onRetry={() => undefined}
        versions={[]}
      />
    );

    expect(markup).toContain('No versions');
  });
});

describe('ActionsView', () => {
  test('renders both action sections', () => {
    const markup = wrap(<ActionsView namespace={NAMESPACE} />);
    expect(markup).toContain('Start workflow');
    expect(markup).toContain('Deploy package');
    expect(markup).toContain('Package archive');
  });
});
