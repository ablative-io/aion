import { useId, useState } from 'react';
import { Link } from 'react-router';

import { workflowDetailHref } from '@/app/routePaths';
import { ErrorState } from '@/components/ErrorState';
import { Button } from '@/components/ui';
import type { ApiClient } from '@/lib/api';
import type { Namespace } from '@/types';

import { useStartWorkflow } from '../hooks/useStartWorkflow';
import { parseJsonInput } from '../lib/jsonInput';

export type StartWorkflowFormProps = {
  namespace: Namespace | null;
  /** Injected client for tests. */
  apiClient?: Pick<ApiClient, 'startWorkflow'> | undefined;
};

const FIELD_CLASS =
  'h-9 rounded-md border border-input bg-transparent px-3 text-sm outline-none focus:border-[var(--text-muted)]';
const AREA_CLASS =
  'min-h-28 rounded-md border border-input bg-transparent px-3 py-2 font-mono text-sm outline-none focus:border-[var(--text-muted)]';

/**
 * Start a workflow run. Namespace-scoped command authority (ADR-022): no deploy
 * grant needed. Provenance is honest — the success panel renders only after the
 * server returns real `workflow_id`/`run_id` values, and deep-links to the run.
 *
 * The namespace is the SELECTED namespace when one exists, otherwise the operator
 * types it free-form: `POST /workflows/start` mints an unseen namespace on use
 * (open policy, `start_mint`), so a fresh server with no namespaces is not a
 * dead-end — you type a namespace and the run creates it. The effective namespace
 * is the selected prop, or the trimmed free-form entry when none is selected.
 */
export function StartWorkflowForm({ namespace, apiClient }: StartWorkflowFormProps) {
  const ids = useFieldIds();
  const [workflowType, setWorkflowType] = useState('');
  const [namespaceEntry, setNamespaceEntry] = useState('');
  const [routingKey, setRoutingKey] = useState('');
  const [taskQueue, setTaskQueue] = useState('');
  const [inputText, setInputText] = useState('');
  const [inputError, setInputError] = useState<string | null>(null);

  // No selected namespace on a fresh server: fall back to the free-form entry,
  // which the start path mints on use. A selected namespace always takes it.
  const trimmedEntry = namespaceEntry.trim();
  const effectiveNamespace: Namespace | null =
    namespace ?? (trimmedEntry.length > 0 ? (trimmedEntry as Namespace) : null);

  const start = useStartWorkflow({ namespace: effectiveNamespace, apiClient });
  const trimmedType = workflowType.trim();
  const canSubmit = effectiveNamespace !== null && trimmedType.length > 0 && !start.isPending;

  function handleSubmit(event: React.FormEvent) {
    event.preventDefault();
    setInputError(null);

    const parsed = parseJsonInput(inputText);
    if (!parsed.ok) {
      setInputError(parsed.error);
      return;
    }

    const trimmedRoutingKey = routingKey.trim();
    const trimmedTaskQueue = taskQueue.trim();
    start.mutate({
      workflowType: trimmedType,
      input: parsed.value,
      ...(trimmedRoutingKey.length === 0 ? {} : { routingKey: trimmedRoutingKey }),
      ...(trimmedTaskQueue.length === 0 ? {} : { taskQueue: trimmedTaskQueue }),
    });
  }

  return (
    <div className="space-y-4">
      <form
        className="grid gap-3 rounded-lg border border-[var(--border-default)] p-4"
        onSubmit={handleSubmit}
      >
        {namespace === null ? (
          <label className="flex flex-col gap-2 font-medium text-sm" htmlFor={ids.namespace}>
            Namespace
            <input
              className={FIELD_CLASS}
              id={ids.namespace}
              placeholder="e.g. orders — created on start if new"
              value={namespaceEntry}
              onChange={(event) => setNamespaceEntry(event.currentTarget.value)}
            />
            <span className="font-normal text-[var(--text-muted)] text-xs">
              No namespace is selected. Type one to target — an unseen namespace is created when the
              run starts.
            </span>
          </label>
        ) : null}
        <label className="flex flex-col gap-2 font-medium text-sm" htmlFor={ids.workflowType}>
          Workflow type
          <input
            className={FIELD_CLASS}
            id={ids.workflowType}
            placeholder="e.g. EmailDigest"
            value={workflowType}
            onChange={(event) => setWorkflowType(event.currentTarget.value)}
          />
        </label>
        <label className="flex flex-col gap-2 font-medium text-sm" htmlFor={ids.input}>
          Input (JSON)
          <textarea
            className={AREA_CLASS}
            id={ids.input}
            placeholder='{} or {"to": "ops@aion"}'
            value={inputText}
            onChange={(event) => setInputText(event.currentTarget.value)}
          />
          <span className="font-normal text-[var(--text-muted)] text-xs">
            Blank sends an empty object. The engine requires a payload.
          </span>
        </label>
        <label className="flex flex-col gap-2 font-medium text-sm" htmlFor={ids.routingKey}>
          Routing key (optional)
          <input
            className={FIELD_CLASS}
            id={ids.routingKey}
            placeholder="steers placement; blank = unsteered"
            value={routingKey}
            onChange={(event) => setRoutingKey(event.currentTarget.value)}
          />
        </label>
        <label className="flex flex-col gap-2 font-medium text-sm" htmlFor={ids.taskQueue}>
          Task queue (optional)
          <input
            className={FIELD_CLASS}
            id={ids.taskQueue}
            placeholder="default activity queue; blank = namespace default"
            value={taskQueue}
            onChange={(event) => setTaskQueue(event.currentTarget.value)}
          />
        </label>
        <div className="flex items-center gap-3">
          <Button disabled={!canSubmit} type="submit">
            {start.isPending ? 'Starting…' : 'Start workflow'}
          </Button>
          {inputError === null ? null : (
            <p className="text-[var(--destructive)] text-sm">{inputError}</p>
          )}
        </div>
      </form>

      <StartOutcome
        error={start.error}
        isError={start.isError}
        namespace={effectiveNamespace}
        result={start.data ?? null}
      />
    </div>
  );
}

export type StartOutcomeProps = {
  isError: boolean;
  error: unknown;
  /** The namespace the run targeted; non-null whenever `result` is present. */
  namespace: Namespace | null;
  result: { workflowId: string; runId: string } | null;
};

export function StartOutcome({ isError, error, namespace, result }: StartOutcomeProps) {
  if (isError) {
    return <ErrorState error={error} title="Start failed" />;
  }

  if (result === null) {
    return null;
  }

  return (
    <div className="rounded-lg border border-[var(--accent-cyan)]/40 bg-[var(--accent-cyan-glow)] p-4">
      <h3 className="font-medium text-[var(--text-primary)] text-sm">Workflow started</h3>
      <dl className="mt-2 space-y-1 text-sm">
        <Detail label="Namespace" value={namespace ?? ''} />
        <Detail label="Workflow ID" value={result.workflowId} />
        <Detail label="Run ID" value={result.runId} />
      </dl>
      <Link
        className="mt-3 inline-block text-[var(--accent-cyan)] text-sm underline-offset-4 hover:underline"
        to={workflowDetailHref(result.workflowId)}
      >
        Open run →
      </Link>
    </div>
  );
}

function Detail({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex gap-2">
      <dt className="text-[var(--text-muted)]">{label}</dt>
      <dd className="font-mono text-[var(--text-primary)]">{value}</dd>
    </div>
  );
}

function useFieldIds() {
  const prefix = useId();

  return {
    namespace: `${prefix}-namespace`,
    workflowType: `${prefix}-workflow-type`,
    input: `${prefix}-input`,
    routingKey: `${prefix}-routing-key`,
    taskQueue: `${prefix}-task-queue`,
  };
}
