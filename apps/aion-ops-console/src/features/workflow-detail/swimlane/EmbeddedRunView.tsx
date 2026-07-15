import { EmptyState } from '@/components/EmptyState';
import type { WorkflowId } from '@/types';

import { useLiveWorkflowEvents } from '../hooks/useLiveWorkflowEvents';
import { useWorkflowHistory } from '../hooks/useWorkflowHistory';
import { WorkflowDetailViewContent } from './WorkflowDetailViewContent';

/**
 * The recursive embedded run view (nested child swim-lanes).
 *
 * Mounted when a child lane is expanded in a parent's swimlane: it composes the
 * SAME data pipeline as the routed detail view — `useWorkflowHistory` backfills
 * the full durable history first (an hour-late expand shows everything), then
 * `useLiveWorkflowEvents` attaches the live per-workflow subscription only after
 * history succeeds and only while the run is non-terminal. Collapsing the lane
 * unmounts this component, which closes the subscription — only expanded lanes
 * ever hold a socket subscription.
 *
 * Recursion is self-contained (this module renders itself for grandchildren, so
 * there is no import cycle) and cycle-guarded: an id already present in its own
 * ancestor chain renders {@link CycleNotice} instead of re-embedding.
 */

/** Pure cycle guard: an id already in its own ancestor chain must not re-embed. */
export function isEmbedCycle(workflowId: string, ancestry: readonly string[]): boolean {
  return ancestry.includes(workflowId);
}

export type EmbeddedRunViewProps = {
  workflowId: WorkflowId;
  namespace: string | null;
  /** Ancestor chain, outermost first; also the recursion/cycle guard. */
  ancestry: readonly string[];
  /**
   * Child ids whose run regions start expanded, propagated through EVERY
   * recursion level (tests only: the SSR suite cannot click the expand toggle,
   * and this is how it proves the REAL recursion — ancestry accumulation and
   * the depth cycle guard — by execution).
   */
  initialExpandedChildren?: readonly string[] | undefined;
};

/** Presentational cycle notice (SSR-testable, no hooks). */
export function CycleNotice({ workflowId }: { workflowId: string }) {
  return (
    <div data-testid="embed-cycle">
      <EmptyState
        description={`Workflow ${workflowId} is already expanded above; open it in its own view instead.`}
        title="Recursive child reference"
      />
    </div>
  );
}

export function EmbeddedRunView({
  workflowId,
  namespace,
  ancestry,
  initialExpandedChildren,
}: EmbeddedRunViewProps) {
  if (isEmbedCycle(workflowId, ancestry)) {
    return <CycleNotice workflowId={workflowId} />;
  }
  return (
    <EmbeddedRunViewBody
      ancestry={ancestry}
      initialExpandedChildren={initialExpandedChildren}
      namespace={namespace}
      workflowId={workflowId}
    />
  );
}

function EmbeddedRunViewBody({
  workflowId,
  namespace,
  ancestry,
  initialExpandedChildren,
}: EmbeddedRunViewProps) {
  // EXACT mirror of the route wrapper's data composition: full durable backfill
  // FIRST, then the live tail attaches only after history succeeds.
  const historyQuery = useWorkflowHistory({ workflowId });
  const live = useLiveWorkflowEvents({
    enabled: historyQuery.isSuccess,
    history: historyQuery.data ?? [],
    workflowId,
  });

  return (
    <WorkflowDetailViewContent
      ancestry={ancestry}
      embedded
      error={historyQuery.error}
      history={live.events}
      initialExpandedChildren={initialExpandedChildren}
      isError={historyQuery.isError}
      isLive={!live.isTerminal && namespace !== null}
      isLoading={historyQuery.isLoading || historyQuery.isPending}
      isTerminal={live.isTerminal}
      namespace={namespace}
      onRetry={() => void historyQuery.refetch()}
      renderChildRun={(childId) => (
        <EmbeddedRunView
          ancestry={[...ancestry, workflowId]}
          initialExpandedChildren={initialExpandedChildren}
          namespace={namespace}
          workflowId={childId}
        />
      )}
      terminalOutcome={live.terminalOutcome}
      timeline={live.timeline}
      workflowId={workflowId}
    />
  );
}
