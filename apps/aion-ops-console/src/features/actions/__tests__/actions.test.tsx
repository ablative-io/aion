import { afterEach, describe, expect, test } from 'bun:test';
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
import { startWorkflowDraftStore } from '../lib/startWorkflowDraft';

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
  test('offers a free-form namespace entry when none is selected (no dead-end)', () => {
    // A fresh server has no namespaces, so `namespace` is null. Instead of a
    // dead-end the form renders a namespace field the operator types into; the
    // start path mints an unseen namespace on use.
    const markup = wrap(<StartWorkflowForm namespace={null} />);
    expect(markup).toContain('Namespace');
    expect(markup).toContain('created when the run starts');
    // The rest of the form is available so the flow is usable end to end.
    expect(markup).toContain('Workflow type');
    expect(markup).toContain('Start workflow');
  });

  test('renders the form fields (no namespace entry) when a namespace is selected', () => {
    const markup = wrap(<StartWorkflowForm namespace={NAMESPACE} />);
    expect(markup).toContain('Workflow type');
    expect(markup).toContain('Input (JSON)');
    expect(markup).toContain('Start workflow');
    // A selected namespace takes precedence, so the free-form hint is absent.
    expect(markup).not.toContain('created when the run starts');
  });
});

describe('StartWorkflowForm drafts', () => {
  afterEach(() => {
    startWorkflowDraftStore.getState().clearDraft();
  });

  test('a half-filled form is restored on return (draft survives unmount)', () => {
    // Simulate the write-through a previous mount performed before navigating away.
    startWorkflowDraftStore.getState().setDraft({
      workflowType: 'DraftedDigest',
      inputText: '{"to": "ops-draft"}',
      routingKey: 'draft-rk-7',
    });

    const markup = wrap(<StartWorkflowForm namespace={NAMESPACE} />);
    expect(markup).toContain('DraftedDigest');
    expect(markup).toContain('ops-draft');
    expect(markup).toContain('draft-rk-7');
  });

  test('the free-form namespace entry is drafted too', () => {
    startWorkflowDraftStore.getState().setDraft({ namespaceEntry: 'drafted-namespace' });

    const markup = wrap(<StartWorkflowForm namespace={null} />);
    expect(markup).toContain('drafted-namespace');
  });

  test('a cleared draft renders a pristine form (the confirmed-start path)', () => {
    startWorkflowDraftStore.getState().setDraft({ workflowType: 'DraftedDigest' });
    // The submit handler clears the draft on a confirmed start; the next
    // mount must not resurrect the consumed draft.
    startWorkflowDraftStore.getState().clearDraft();

    const markup = wrap(<StartWorkflowForm namespace={NAMESPACE} />);
    expect(markup).not.toContain('DraftedDigest');
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
  test('renders the deploy section only when the runtime deploy grant is present', () => {
    const granted = wrap(<ActionsView namespace={NAMESPACE} deployGranted={true} />);
    expect(granted).toContain('Start workflow');
    expect(granted).toContain('Deploy package');
    expect(granted).toContain('Package archive');
  });

  test('hides the deploy affordance when the caller is not deploy-granted', () => {
    // deployGranted defaults to false: an ungranted caller never sees the panel.
    const ungranted = wrap(<ActionsView namespace={NAMESPACE} />);
    expect(ungranted).toContain('Start workflow');
    expect(ungranted).not.toContain('Deploy package');
    expect(ungranted).not.toContain('Package archive');
  });
});
