import { useId, useState } from 'react';
import { useNavigate } from 'react-router';

import { assistantSessionHref } from '@/app/routePaths';
import { ErrorState } from '@/components/ErrorState';
import { Button } from '@/components/ui';
import { useStartWorkflow } from '@/features/actions';
import type { ApiClient } from '@/lib/api';
import type { Namespace } from '@/types';

import { ASSISTANT_WORKFLOW_TYPE, assistantStartInput } from '../lib/contract';

const FIELD_CLASS =
  'h-9 rounded-md border border-input bg-transparent px-3 text-sm outline-none focus:border-muted-foreground';
const AREA_CLASS =
  'min-h-20 rounded-md border border-input bg-transparent px-3 py-2 text-sm outline-none focus:border-muted-foreground';

export type NewAssistantSessionFormProps = {
  namespace: Namespace | null;
  /** Injected client (tests); defaults to the configured live client. */
  apiClient?: Pick<ApiClient, 'startWorkflow'> | undefined;
};

/**
 * Start a new assistant session: `POST /workflows/start` with the contract
 * input `{ objective, repo_path }`, then navigate to the session the SERVER
 * confirmed (provenance is never fabricated — a failed start surfaces as
 * visible error state and navigates nowhere).
 *
 * Field drafts are plain component state for now: the parallel state track's
 * draft-persistence helper is not visible in this worktree — swap it in at
 * merge so a navigation away stops eating a half-written objective.
 */
export function NewAssistantSessionForm({ namespace, apiClient }: NewAssistantSessionFormProps) {
  const ids = useFieldIds();
  const navigate = useNavigate();
  const [objective, setObjective] = useState('');
  const [repoPath, setRepoPath] = useState('');
  const start = useStartWorkflow({ namespace, apiClient });

  const canSubmit = namespace !== null && objective.trim().length > 0 && !start.isPending;

  function handleSubmit(event: React.FormEvent) {
    event.preventDefault();
    if (!canSubmit) {
      return;
    }
    start
      .mutateAsync({
        workflowType: ASSISTANT_WORKFLOW_TYPE,
        input: assistantStartInput(objective.trim(), repoPath.trim()),
      })
      .then((result) => navigate(assistantSessionHref(result.workflowId)))
      .catch(() => {
        // The failure is already visible mutation state (start.isError below).
      });
  }

  return (
    <div className="space-y-3">
      <form className="grid gap-3 rounded-lg border border-border p-4" onSubmit={handleSubmit}>
        <label className="flex flex-col gap-2 font-medium text-sm" htmlFor={ids.objective}>
          Objective
          <textarea
            className={AREA_CLASS}
            id={ids.objective}
            onChange={(event) => setObjective(event.currentTarget.value)}
            placeholder="What should the assistant work on?"
            value={objective}
          />
        </label>
        <label className="flex flex-col gap-2 font-medium text-sm" htmlFor={ids.repoPath}>
          Repository path (optional)
          <input
            className={FIELD_CLASS}
            id={ids.repoPath}
            onChange={(event) => setRepoPath(event.currentTarget.value)}
            placeholder="/path/to/repo — blank pins no repo"
            value={repoPath}
          />
        </label>
        <div className="flex items-center gap-3">
          <Button disabled={!canSubmit} type="submit">
            {start.isPending ? 'Starting…' : 'Start session'}
          </Button>
          {namespace === null ? (
            <p className="text-muted-foreground text-xs">Select a namespace to start a session.</p>
          ) : null}
        </div>
      </form>
      {start.isError ? (
        <ErrorState error={start.error} title="Could not start the session" />
      ) : null}
    </div>
  );
}

function useFieldIds() {
  const prefix = useId();
  return {
    objective: `${prefix}-objective`,
    repoPath: `${prefix}-repo-path`,
  };
}
