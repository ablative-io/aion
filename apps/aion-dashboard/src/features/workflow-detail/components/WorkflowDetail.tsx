import { EmptyState } from '@/components/EmptyState';
import { ErrorState } from '@/components/ErrorState';
import { LoadingSkeleton } from '@/components/LoadingSkeleton';
import type { Event } from '@/types';

import { useWorkflowHistory } from '../hooks/useWorkflowHistory';
import { projectTimeline } from '../lib/timeline';
import type { WorkflowDetailProps } from '../types';
import { EventTimeline } from './EventTimeline';

type WorkflowDetailContentProps = WorkflowDetailProps & {
  history: readonly Event[];
  isError: boolean;
  isLoading: boolean;
  error: unknown;
  onRetry?: () => void;
};

function WorkflowDetail({ workflowId, namespace }: WorkflowDetailProps) {
  const historyQuery = useWorkflowHistory(namespace, workflowId);

  return (
    <WorkflowDetailContent
      error={historyQuery.error}
      history={historyQuery.data ?? []}
      isError={historyQuery.isError}
      isLoading={historyQuery.isLoading || historyQuery.isPending}
      namespace={namespace}
      onRetry={() => void historyQuery.refetch()}
      workflowId={workflowId}
    />
  );
}

function WorkflowDetailContent({
  workflowId,
  namespace,
  history,
  isError,
  isLoading,
  error,
  onRetry,
}: WorkflowDetailContentProps) {
  if (namespace === null) {
    return (
      <EmptyState
        description="Select a namespace to scope the history request."
        title="No namespace selected"
      />
    );
  }

  if (isLoading) {
    return <LoadingSkeleton />;
  }

  if (isError) {
    return <ErrorState error={error} onRetry={onRetry} title="Could not load workflow history" />;
  }

  if (history.length === 0) {
    return (
      <EmptyState
        description="This workflow has no events yet."
        title="This workflow has no events yet"
      />
    );
  }

  return (
    <section className="space-y-4">
      <header>
        <p className="text-[var(--text-muted)] text-sm">Namespace {namespace}</p>
        <h1 className="font-semibold text-2xl text-[var(--text-primary)]">Workflow {workflowId}</h1>
      </header>
      <EventTimeline entries={projectTimeline(history)} />
    </section>
  );
}

export { WorkflowDetail, WorkflowDetailContent };
