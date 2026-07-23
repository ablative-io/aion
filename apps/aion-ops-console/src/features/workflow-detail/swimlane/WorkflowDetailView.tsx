import { useLiveWorkflowEvents } from '../hooks/useLiveWorkflowEvents';
import { useWorkflowHistory } from '../hooks/useWorkflowHistory';
import { useWorkflowSelectionParams } from '../hooks/useWorkflowSelectionParams';
import type { WorkflowDetailProps } from '../types';
import { WorkflowDetailViewContent } from './WorkflowDetailViewContent';

/**
 * Detail view wrapper that adds a List ⇄ Swimlane toggle on top of the history +
 * live projection (VISION §4.1; PLAN S4). Reuses the history + live hooks, the
 * projection engine, and EventTimeline. The timeline keeps FULL width; selecting a
 * bar opens the bottom-docked, morphing DetailSheet BELOW it (not a
 * right-side panel that compresses the chart). Selection + view mode are URL
 * navigation state (see {@link useWorkflowSelectionParams}); the router-connected
 * wrapper owns them and the presentational content falls back to internal state so
 * it still renders without a router. The AttemptNavigator beneath enumerates
 * the durable attempt list. S0's `/workflows/:id` route renders this wrapper.
 */
function WorkflowDetailView({ workflowId, namespace }: WorkflowDetailProps) {
  const historyQuery = useWorkflowHistory({ workflowId });
  const live = useLiveWorkflowEvents({
    enabled: historyQuery.isSuccess,
    history: historyQuery.data ?? [],
    workflowId,
  });
  // Selection + view mode are URL navigation state (PART 3); the router-connected
  // wrapper owns them and hands the resolved values + setters to the presentational
  // content, which stays renderable without a router (SSR tests).
  const selection = useWorkflowSelectionParams();

  return (
    <WorkflowDetailViewContent
      error={historyQuery.error}
      hasEarlier={historyQuery.hasEarlier}
      history={live.events}
      isError={historyQuery.isError}
      isFetchingEarlier={historyQuery.isFetchingEarlier}
      isLive={!live.isTerminal && namespace !== null}
      isLoading={historyQuery.isLoading || historyQuery.isPending}
      isTerminal={live.isTerminal}
      mode={selection.mode}
      namespace={namespace}
      onLoadEarlier={() => void historyQuery.loadEarlier()}
      onModeChange={selection.setMode}
      onRetry={() => void historyQuery.refetch()}
      onScrubChange={selection.setScrub}
      onSelectSequence={selection.setSelectedSequence}
      scrubPosition={selection.scrubPosition}
      selectedSequence={selection.selectedSequence}
      terminalOutcome={live.terminalOutcome}
      timeline={live.timeline}
      workflowId={workflowId}
    />
  );
}

// Import-stability re-export: `Swimlane.test.tsx` and `swimlane/index.ts` import
// the presentational content from this module.
export { WorkflowDetailViewContent } from './WorkflowDetailViewContent';
export { WorkflowDetailView };
