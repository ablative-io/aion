import { useCallback, useMemo, useRef, useState } from 'react';

import { EmptyState } from '@/components/EmptyState';
import { ErrorState } from '@/components/ErrorState';
import { LoadingSkeleton } from '@/components/LoadingSkeleton';
import { Badge, Button } from '@/components/ui';
import { cn } from '@/lib/utils';
import type { Event, WorkflowId } from '@/types';

import { EventTimeline } from '../components/EventTimeline';
import type { projectTimeline } from '../lib/timeline';
import { deriveFailureContext, ReopenDiff } from '../reopen';
import type { LifecycleOutcome, TimelineEntry, WorkflowDetailProps } from '../types';
import { AttemptNavigator } from './AttemptNavigator';
import { DetailSheet } from './DetailSheet';
import { Swimlane, type SwimlaneSelection } from './Swimlane';
import { resolveSelectionSurface } from './selectionSurface';

type ViewMode = 'swimlane' | 'list';

export type ContentProps = WorkflowDetailProps & {
  timeline: ReturnType<typeof projectTimeline>;
  /** Live-merged raw history, used for the reopen-diff preview. */
  history: readonly Event[];
  isError: boolean;
  isLoading: boolean;
  error: unknown;
  isTerminal: boolean;
  terminalOutcome: LifecycleOutcome | null;
  isLive: boolean;
  onRetry?: () => void;
  /** Initial mode, overridable for tests (uncontrolled fallback). */
  initialMode?: ViewMode;
  /** Open the reopen-diff panel on first render (tests only). */
  initialReopenOpen?: boolean;
  /**
   * Selection + view mode are optionally CONTROLLED by the router-connected
   * wrapper (URL state, PART 3). When a setter is omitted the field falls back to
   * internal `useState`, so this presentational component renders without a router
   * (the SSR unit tests pass none of these).
   */
  selectedSequence?: number | null;
  onSelectSequence?: (sequence: number | null) => void;
  mode?: ViewMode;
  onModeChange?: (mode: ViewMode) => void;
  scrubPosition?: number | null;
  onScrubChange?: (scrubPosition: number | null) => void;
};

/**
 * A workflow is reopenable exactly when the server allows it: a terminal Failed
 * or Cancelled run (AD-012). We gate on the projected terminal OUTCOME, not on an
 * already-recorded `WorkflowReopened` event — the affordance must appear for a
 * fresh failure that has never been reopened.
 */
function isReopenable(isTerminal: boolean, outcome: LifecycleOutcome | null): boolean {
  return isTerminal && (outcome === 'failed' || outcome === 'cancelled');
}

type SelectionStateOptions = {
  initialMode: ViewMode;
  modeProp: ViewMode | undefined;
  onModeChange: ((mode: ViewMode) => void) | undefined;
  selectedSequenceProp: number | null | undefined;
  onSelectSequence: ((sequence: number | null) => void) | undefined;
  scrubPositionProp: number | null | undefined;
  onScrubChange: ((scrubPosition: number | null) => void) | undefined;
};

/**
 * Resolve mode / selectedSequence / scrubSeq as CONTROLLED (a setter was supplied
 * by the URL-backed wrapper) or fall back to internal `useState` (the SSR unit
 * tests, which render {@link WorkflowDetailViewContent} with no router). Keeping
 * this out of the component body keeps its branching readable.
 */
function useSelectionState({
  initialMode,
  modeProp,
  onModeChange,
  selectedSequenceProp,
  onSelectSequence,
  scrubPositionProp,
  onScrubChange,
}: SelectionStateOptions) {
  const [modeState, setModeState] = useState<ViewMode>(initialMode);
  const [selectedSequenceState, setSelectedSequenceState] = useState<number | null>(null);
  const [scrubSeqState, setScrubSeqState] = useState<number | null>(null);

  return {
    mode: onModeChange ? (modeProp ?? 'swimlane') : modeState,
    setMode: onModeChange ?? setModeState,
    selectedSequence: onSelectSequence ? (selectedSequenceProp ?? null) : selectedSequenceState,
    setSelectedSequence: onSelectSequence ?? setSelectedSequenceState,
    scrubPosition: onScrubChange ? (scrubPositionProp ?? null) : scrubSeqState,
    setScrubPosition: onScrubChange ?? setScrubSeqState,
  };
}

function WorkflowDetailViewContent({
  workflowId,
  namespace,
  timeline,
  history,
  isError,
  isLoading,
  error,
  isTerminal,
  terminalOutcome,
  isLive,
  onRetry,
  initialMode = 'swimlane',
  initialReopenOpen = false,
  selectedSequence: selectedSequenceProp,
  onSelectSequence,
  mode: modeProp,
  onModeChange,
  scrubPosition: scrubPositionProp,
  onScrubChange,
}: ContentProps) {
  const { mode, setMode, selectedSequence, setSelectedSequence, scrubPosition, setScrubPosition } =
    useSelectionState({
      initialMode,
      modeProp,
      onModeChange,
      onScrubChange,
      onSelectSequence,
      scrubPositionProp,
      selectedSequenceProp,
    });

  const [reopenOpen, setReopenOpen] = useState(initialReopenOpen);
  const [scopedSelection, setScopedSelection] = useState<SwimlaneSelection | null>(null);
  // The clicked bar's x-origin (relative to the timeline container's left edge),
  // so the bottom-docked sheet morphs out of the bar. Ephemeral, never URL state.
  const [sheetOriginX, setSheetOriginX] = useState<number | null>(null);
  const timelineRef = useRef<HTMLDivElement | null>(null);
  const reopenable = isReopenable(isTerminal, terminalOutcome);
  const failureContext = useMemo(() => deriveFailureContext(history), [history]);
  const selectionSurface = useMemo(
    () => resolveSelectionSurface(workflowId, timeline, selectedSequence, scopedSelection),
    [scopedSelection, selectedSequence, timeline, workflowId]
  );
  const selectedTimeline = selectionSurface.timeline;
  const selectedWorkflowId = selectionSurface.workflowId;
  const selectedEntry = selectionSurface.entry;
  const clusterEntries = selectionSurface.clusterEntries;
  const activeScopedSelection = selectionSurface.scopedSelection;

  const selectFromBar = useCallback(
    (selection: SwimlaneSelection, origin?: { x: number }) => {
      const container = timelineRef.current;
      setSheetOriginX(
        origin && container ? origin.x - container.getBoundingClientRect().left : null
      );
      setScopedSelection(selection);
      setSelectedSequence(selection.sequence);
    },
    [setSelectedSequence]
  );
  const selectFromList = useCallback(
    (sequence: number) => {
      setSheetOriginX(null);
      setScopedSelection(null);
      setSelectedSequence(sequence);
    },
    [setSelectedSequence]
  );
  const selectTimelineEntry = useCallback(
    (entry: TimelineEntry) => selectFromList(entry.sequence),
    [selectFromList]
  );
  const closeSelection = useCallback(() => {
    setScopedSelection(null);
    setSelectedSequence(null);
  }, [setSelectedSequence]);
  const selectClusterEntry = useCallback(
    (sequence: number) => {
      if (activeScopedSelection !== null) {
        setScopedSelection({ ...activeScopedSelection, sequence });
      }
      setSelectedSequence(sequence);
    },
    [activeScopedSelection, setSelectedSequence]
  );

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

  if (timeline.length === 0) {
    return (
      <EmptyState
        description="This workflow has no events yet."
        title="This workflow has no events yet"
      />
    );
  }

  return (
    <section className="space-y-4">
      <header className="space-y-2">
        <p className="text-muted-foreground text-sm">Namespace {namespace}</p>
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="flex flex-wrap items-center gap-3">
            <h1 className="font-semibold text-2xl text-foreground">Workflow {workflowId}</h1>
            <StatusBadge
              isLive={isLive}
              isTerminal={isTerminal}
              terminalOutcome={terminalOutcome}
            />
          </div>
          <div className="flex items-center gap-2">
            {reopenable ? (
              <Button
                className="h-7 px-3 text-xs"
                data-testid="reopen-open"
                onClick={() => setReopenOpen(true)}
                type="button"
                variant="outline"
              >
                Reopen
              </Button>
            ) : null}
            <ViewToggle mode={mode} onChange={setMode} />
          </div>
        </div>
        {reopenable ? <FailureContextPanel context={failureContext} /> : null}
      </header>
      {/* The timeline keeps FULL width; the detail sheet docks BELOW it (PART 2)
          rather than a right-side panel that compresses the chart. */}
      <div className="space-y-3">
        <div className="min-w-0 space-y-3" ref={timelineRef}>
          {mode === 'swimlane' ? (
            <Swimlane
              entries={timeline}
              isRunning={!isTerminal}
              loadChildren
              onScrubChange={setScrubPosition}
              onSelect={selectFromBar}
              scrubPosition={scrubPosition}
              selectedSequence={selectedSequence}
              selectedWorkflowId={selectedWorkflowId}
              workflowId={workflowId}
            />
          ) : (
            <EventTimeline
              entries={timeline}
              onSelect={selectTimelineEntry}
              selectedSequence={selectedSequence}
            />
          )}
        </div>
        <div data-testid="selection-scope" data-workflow-id={selectedWorkflowId}>
          <DetailSheet
            clusterEntries={clusterEntries}
            entry={selectedEntry}
            onClose={closeSelection}
            onSelectClusterEntry={selectClusterEntry}
            originX={sheetOriginX}
          />
          <AttemptNavigator
            key={selectedWorkflowId}
            namespace={namespace}
            timeline={selectedTimeline}
            workflowId={selectedWorkflowId as WorkflowId}
          />
        </div>
      </div>
      {reopenOpen ? (
        <ReopenDiff
          history={history}
          namespace={namespace}
          onClose={() => setReopenOpen(false)}
          workflowId={workflowId}
        />
      ) : null}
    </section>
  );
}

/**
 * The failure context a user reads before reopening: the failed step + reason,
 * derived from history (mirrors the `failed_step`/`failure_reason` the list
 * carries). Rendered ONLY when history explains the failure — a bare gate with no
 * derivable context (e.g. a cancellation with an empty reason) shows nothing.
 */
function FailureContextPanel({ context }: { context: ReturnType<typeof deriveFailureContext> }) {
  if (context === null || (context.failedStep === null && context.failureReason === null)) {
    return null;
  }

  return (
    <dl
      className="flex flex-col gap-1 rounded-md border border-destructive/30 bg-destructive/5 p-3 text-sm"
      data-testid="failure-context"
    >
      {context.failedStep !== null ? (
        <div className="flex gap-2">
          <dt className="text-muted-foreground">Failed step</dt>
          <dd className="font-mono text-foreground">{context.failedStep}</dd>
        </div>
      ) : null}
      {context.failureReason !== null ? (
        <div className="flex gap-2">
          <dt className="text-muted-foreground">Reason</dt>
          <dd className="text-secondary-foreground">{context.failureReason}</dd>
        </div>
      ) : null}
    </dl>
  );
}

function ViewToggle({ mode, onChange }: { mode: ViewMode; onChange: (mode: ViewMode) => void }) {
  return (
    <fieldset
      aria-label="Detail view mode"
      className="inline-flex rounded-lg border border-border p-0.5"
    >
      <ToggleButton
        active={mode === 'swimlane'}
        label="Swimlane"
        onClick={() => onChange('swimlane')}
      />
      <ToggleButton active={mode === 'list'} label="List" onClick={() => onChange('list')} />
    </fieldset>
  );
}

function ToggleButton({
  active,
  label,
  onClick,
}: {
  active: boolean;
  label: string;
  onClick: () => void;
}) {
  return (
    <Button
      aria-pressed={active}
      className={cn('h-7 px-3 text-xs', active && 'bg-surface-hover')}
      onClick={onClick}
      type="button"
      variant="ghost"
    >
      {label}
    </Button>
  );
}

function StatusBadge({
  isLive,
  isTerminal,
  terminalOutcome,
}: {
  isLive: boolean;
  isTerminal: boolean;
  terminalOutcome: LifecycleOutcome | null;
}) {
  if (isTerminal && terminalOutcome) {
    return (
      <Badge variant={terminalOutcome === 'completed' ? 'default' : 'destructive'}>
        {`Terminal: ${terminalOutcome}`}
      </Badge>
    );
  }

  if (isLive) {
    return <Badge variant="secondary">Live</Badge>;
  }

  return <Badge variant="outline">Running</Badge>;
}

export { WorkflowDetailViewContent };
