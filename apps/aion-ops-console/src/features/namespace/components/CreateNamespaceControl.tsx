import { useId, useState } from 'react';

import { Button } from '@/components/ui';
import type { CreateNamespaceResult } from '@/lib/api';
import { createConfiguredApiClient } from '@/lib/config';
import { cn } from '@/lib/utils';

/**
 * The operator "create namespace" affordance for the registry panel.
 *
 * It calls the REAL `POST /namespaces` (never a stub) through a namespace-scoped
 * client so the `x-aion-namespaces` grant header includes the target — the same
 * grant the server's `authorize_namespace` check needs. On success it does NOT
 * optimistically inject a row: the server emits a `NamespaceCreated` cluster
 * delta which the panel's `useNamespaceRegistry` hook folds live, so the socket
 * stays the source of truth. It calls `onCreated` so the parent can refresh the
 * react-query namespace list that drives the selector (which is fed by the REST
 * list, not the socket).
 *
 * Failure is surfaced honestly: the empty-name case is caught client-side before
 * any request, and any server rejection (400 empty name, 403 denied grant, other
 * 4xx) is rendered from the thrown {@link ApiError} — never swallowed.
 */

/** The create action; injectable for tests, defaults to the real REST call. */
export type CreateNamespace = (name: string) => Promise<CreateNamespaceResult>;

function defaultCreateNamespace(name: string): Promise<CreateNamespaceResult> {
  // Scope the client to the target name so the namespace header carries it; in
  // auth-off operator mode the server authorizes regardless, and under real auth
  // the caller must hold the grant, which this scoping supplies.
  return createConfiguredApiClient({ namespace: name }).createNamespace(name);
}

export type CreateNamespaceControlProps = {
  /** Override the create action (tests); defaults to the real `POST /namespaces`. */
  onCreate?: CreateNamespace;
  /**
   * Called after a create resolves (minted OR idempotent already-existed) with the
   * durable name, so the parent can refresh the selector's namespace list. The
   * live registry row still arrives via the `NamespaceCreated` socket delta.
   */
  onCreated?: (result: CreateNamespaceResult) => void;
};

type Feedback = { kind: 'error' | 'created' | 'existed'; message: string };

export function CreateNamespaceControl({ onCreate, onCreated }: CreateNamespaceControlProps) {
  const inputId = useId();
  const create = onCreate ?? defaultCreateNamespace;
  const [name, setName] = useState('');
  const [busy, setBusy] = useState(false);
  const [feedback, setFeedback] = useState<Feedback | null>(null);

  const trimmed = name.trim();
  const canSubmit = trimmed.length > 0 && !busy;

  function submit(event: React.FormEvent): void {
    event.preventDefault();
    if (trimmed.length === 0) {
      setFeedback({ kind: 'error', message: 'Namespace name must not be empty.' });
      return;
    }
    setBusy(true);
    setFeedback(null);
    create(trimmed)
      .then((result) => {
        setFeedback(
          result.created
            ? { kind: 'created', message: `Namespace "${result.name}" created.` }
            : { kind: 'existed', message: `Namespace "${result.name}" already existed.` }
        );
        setName('');
        onCreated?.(result);
      })
      .catch((cause: unknown) => {
        setFeedback({
          kind: 'error',
          message: cause instanceof Error ? cause.message : 'Failed to create namespace.',
        });
      })
      .finally(() => setBusy(false));
  }

  return (
    <form
      className="flex flex-wrap items-end gap-2 rounded-lg border border-[var(--border-default)] p-3"
      onSubmit={submit}
      aria-label="Create namespace"
    >
      <label className="flex flex-col gap-1 font-medium text-sm" htmlFor={inputId}>
        Create namespace
        <input
          id={inputId}
          type="text"
          value={name}
          onChange={(event) => setName(event.currentTarget.value)}
          placeholder="e.g. orders"
          aria-label="New namespace name"
          className={cn(
            'h-9 w-56 rounded-lg border bg-transparent px-3 font-mono text-sm',
            'border-[var(--border-default)] text-[var(--text-primary)]',
            'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--border-focus)]'
          )}
        />
      </label>
      <Button type="submit" size="sm" disabled={!canSubmit}>
        {busy ? 'Creating…' : 'Create'}
      </Button>
      {feedback === null ? null : (
        <p
          className={cn(
            'w-full text-xs',
            feedback.kind === 'error' ? 'text-destructive' : 'text-[var(--text-muted)]'
          )}
          role={feedback.kind === 'error' ? 'alert' : 'status'}
        >
          {feedback.message}
        </p>
      )}
    </form>
  );
}
