import { useMemo, useState } from 'react';

import { EmptyState } from '@/components/EmptyState';
import { ErrorState } from '@/components/ErrorState';
import { LoadingSkeleton } from '@/components/LoadingSkeleton';
import { Button } from '@/components/ui';
import type { AttemptCapabilities } from '@/lib/api';
import type { TranscriptTarget } from '@/lib/api/transcript-stream';
import { cn } from '@/lib/utils';
import type { Namespace, WorkflowId } from '@/types';

import { useActivityAttempts } from '../hooks/useActivityAttempts';
import { InterventionControls } from './InterventionControls';
import { TranscriptPanel } from './TranscriptPanel';

/**
 * Agent observability + intervention view for one workflow (NOI-7).
 *
 * Enumerates the workflow's LIVE intervenable activity attempts (each with its
 * advertised capabilities), lets the operator pick one, streams that attempt's
 * transcript live, and offers capability-gated intervention controls. The
 * enumeration is a user-triggered REST read (with an explicit refresh), while the
 * transcript is socket-first and live. Nothing is mocked and nothing polls.
 */

export type AttemptTranscriptViewProps = {
  workflowId: WorkflowId;
  namespace: Namespace | null;
};

/** A stable identity string for an attempt row (activity + attempt). */
export function attemptId(attempt: AttemptCapabilities): string {
  return `${attempt.activityId}:${attempt.attempt}`;
}

/**
 * Resolve the effective attempt selection. A manual pick wins while it still
 * exists; otherwise, when the enumeration returned exactly ONE live attempt it is
 * auto-selected (there is nothing else to choose, so "No attempt selected" would
 * just be friction). With several attempts and no pick, nothing is selected.
 */
export function resolveSelectedAttempt(
  attempts: AttemptCapabilities[],
  selectedId: string | null
): AttemptCapabilities | null {
  const manual =
    selectedId === null
      ? null
      : (attempts.find((attempt) => attemptId(attempt) === selectedId) ?? null);
  if (manual !== null) {
    return manual;
  }
  return attempts.length === 1 ? (attempts[0] ?? null) : null;
}

export function AttemptTranscriptView({ workflowId, namespace }: AttemptTranscriptViewProps) {
  const { attempts, loadState, loadError, refresh } = useActivityAttempts({
    workflowId,
    namespace,
  });
  const [selectedId, setSelectedId] = useState<string | null>(null);

  const selected = useMemo(
    () => resolveSelectedAttempt(attempts, selectedId),
    [attempts, selectedId]
  );

  const target: TranscriptTarget | null = useMemo(() => {
    if (selected === null || namespace === null) {
      return null;
    }
    return {
      namespace,
      workflowId,
      activityId: selected.activityId,
      attempt: selected.attempt,
    };
  }, [selected, namespace, workflowId]);

  if (namespace === null) {
    return (
      <EmptyState
        description="Select a namespace to enumerate this workflow's live agent attempts."
        title="No namespace selected"
      />
    );
  }

  return (
    <section className="flex flex-col gap-3">
      <header className="flex items-center justify-between gap-2">
        <h2 className="font-semibold text-foreground text-sm">Agent attempts</h2>
        <Button className="h-7 px-3 text-xs" onClick={refresh} type="button" variant="outline">
          Refresh
        </Button>
      </header>
      <AttemptList
        attempts={attempts}
        loadError={loadError}
        loadState={loadState}
        onRetry={refresh}
        onSelect={setSelectedId}
        selectedId={selected === null ? null : attemptId(selected)}
      />
      <div className="flex flex-col gap-3 lg:flex-row lg:items-start">
        <div className="min-w-0 flex-1">
          <TranscriptPanel target={target} />
        </div>
        {selected === null ? null : (
          <div className="w-full lg:w-72">
            <InterventionControls
              attempt={selected}
              namespace={namespace}
              workflowId={workflowId}
            />
          </div>
        )}
      </div>
    </section>
  );
}

type AttemptListProps = {
  attempts: AttemptCapabilities[];
  loadState: ReturnType<typeof useActivityAttempts>['loadState'];
  loadError: Error | null;
  selectedId: string | null;
  onSelect: (id: string) => void;
  onRetry: () => void;
};

function AttemptList({
  attempts,
  loadState,
  loadError,
  selectedId,
  onSelect,
  onRetry,
}: AttemptListProps) {
  if (loadState === 'loading' || loadState === 'idle') {
    return <LoadingSkeleton />;
  }
  if (loadState === 'error') {
    return <ErrorState error={loadError} onRetry={onRetry} title="Could not load agent attempts" />;
  }
  if (attempts.length === 0) {
    return (
      <EmptyState
        description="This workflow has no live agent attempt to observe right now."
        title="No live attempts"
      />
    );
  }

  return (
    <ul className="flex flex-wrap gap-2" data-testid="attempt-list">
      {attempts.map((attempt) => {
        const id = attemptId(attempt);
        return (
          <li key={id}>
            <Button
              aria-pressed={selectedId === id}
              className={cn('h-7 px-3 text-xs', selectedId === id && 'bg-surface-hover')}
              onClick={() => onSelect(id)}
              type="button"
              variant="ghost"
            >
              {`activity ${attempt.activityId} · attempt ${attempt.attempt}`}
              {attempt.capabilities.supported.length === 0 ? ' (observe-only)' : ''}
            </Button>
          </li>
        );
      })}
    </ul>
  );
}
