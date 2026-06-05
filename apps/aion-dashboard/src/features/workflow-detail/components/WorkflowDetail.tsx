import { EmptyState } from '@/components/EmptyState';
import { ErrorState } from '@/components/ErrorState';
import { LoadingSkeleton } from '@/components/LoadingSkeleton';
import { ConnectionIndicator } from '@/features/live-feed';
import type { Event } from '@/types';

import { useLiveWorkflowEvents } from '../hooks/useLiveWorkflowEvents';
import { useWorkflowHistory } from '../hooks/useWorkflowHistory';
import type { LifecycleOutcome, TimelineEntry, WorkflowDetailProps } from '../types';
import { EventTimeline } from './EventTimeline';

type WorkflowDetailContentProps = WorkflowDetailProps & {
  history: readonly Event[];
  isError: boolean;
  isLoading: boolean;
  error: unknown;
  isTerminal?: boolean;
  onRetry?: () => void;
  terminalOutcome?: LifecycleOutcome | null;
  timeline?: readonly TimelineEntry[];
};

function WorkflowDetail({ workflowId, namespace }: WorkflowDetailProps) {
  const historyQuery = useWorkflowHistory({ workflowId });
  const liveEvents = useLiveWorkflowEvents({
    enabled: historyQuery.isSuccess,
    history: historyQuery.data ?? [],
    onResync: () => void historyQuery.refetch(),
    workflowId,
  });

  return (
    <WorkflowDetailContent
      error={historyQuery.error}
      history={liveEvents.events}
      isError={historyQuery.isError}
      isTerminal={liveEvents.isTerminal}
      terminalOutcome={liveEvents.terminalOutcome}
      timeline={liveEvents.timeline}
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
  isTerminal = false,
  onRetry,
  terminalOutcome = null,
  timeline,
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
      <header className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <p className="text-[var(--text-muted)] text-sm">Namespace {namespace}</p>
          <h1 className="font-semibold text-2xl text-[var(--text-primary)]">Workflow {workflowId}</h1>
          {isTerminal && terminalOutcome !== null ? (
            <p className="text-[var(--text-muted)] text-sm">Terminal outcome: {terminalOutcome}</p>
          ) : null}
        </div>
        <ConnectionIndicator />
      </header>
      <EventTimeline entries={timeline} events={timeline === undefined ? history : undefined} />
    </section>
  );
}

export { WorkflowDetail, WorkflowDetailContent };
