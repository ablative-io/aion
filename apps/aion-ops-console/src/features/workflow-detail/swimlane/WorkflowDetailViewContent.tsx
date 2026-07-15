import { Fragment, type ReactNode, useMemo, useRef, useState } from 'react';
import { Link } from 'react-router';

import { workflowDetailHref } from '@/app/routePaths';
import { EmptyState } from '@/components/EmptyState';
import { ErrorState } from '@/components/ErrorState';
import { LoadingSkeleton } from '@/components/LoadingSkeleton';
import { Badge, Button } from '@/components/ui';
import { cn } from '@/lib/utils';
import type { Event } from '@/types';

import { EventTimeline } from '../components/EventTimeline';
import type { projectTimeline } from '../lib/timeline';
import { deriveFailureContext, ReopenDiff } from '../reopen';
import type { LifecycleOutcome, WorkflowDetailProps } from '../types';
import { AttemptNavigator } from './AttemptNavigator';
import { DetailSheet } from './DetailSheet';
import { Scrubber } from './Scrubber';
import { Swimlane } from './Swimlane';

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
   * Render the compact ancestry header (an embedded child run) instead of the
   * page `<h1>` header. The full body — swimlane, timeline, detail sheet,
   * attempt navigator, reopen — renders in BOTH modes: an embedded run view is
   * the real component, not a lite fork.
   */
  embedded?: boolean;
  /** Ancestor workflow-id chain, outermost first; empty/omitted at the root. */
  ancestry?: readonly string[] | undefined;
  /**
   * Injected recursive embed point for child lanes (kept as an injection, not a
   * direct import, so the module graph stays acyclic). When omitted the
   * swimlane renders no expansion affordance.
   */
  renderChildRun?: ((childWorkflowId: string) => ReactNode) | undefined;
  /**
   * Child workflow ids whose run regions start expanded — forwarded to
   * {@link Swimlane} (tests only, same convention as its own prop: the SSR
   * suite cannot click the expand toggle).
   */
  initialExpandedChildren?: readonly string[] | undefined;
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
  scrubSeq?: number | null;
  onScrubChange?: (scrubSeq: number | null) => void;
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
  scrubSeqProp: number | null | undefined;
  onScrubChange: ((scrubSeq: number | null) => void) | undefined;
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
  scrubSeqProp,
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
    scrubSeq: onScrubChange ? (scrubSeqProp ?? null) : scrubSeqState,
    setScrubSeq: onScrubChange ?? setScrubSeqState,
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
  embedded = false,
  ancestry,
  renderChildRun,
  initialExpandedChildren,
  selectedSequence: selectedSequenceProp,
  onSelectSequence,
  mode: modeProp,
  onModeChange,
  scrubSeq: scrubSeqProp,
  onScrubChange,
}: ContentProps) {
  const { mode, setMode, selectedSequence, setSelectedSequence, scrubSeq, setScrubSeq } =
    useSelectionState({
      initialMode,
      modeProp,
      onModeChange,
      onScrubChange,
      onSelectSequence,
      scrubSeqProp,
      selectedSequenceProp,
    });

  const [reopenOpen, setReopenOpen] = useState(initialReopenOpen);
  // The clicked bar's x-origin (relative to the timeline container's left edge),
  // so the bottom-docked sheet morphs out of the bar. Ephemeral, never URL state.
  const [sheetOriginX, setSheetOriginX] = useState<number | null>(null);
  const timelineRef = useRef<HTMLDivElement | null>(null);
  const reopenable = isReopenable(isTerminal, terminalOutcome);
  const failureContext = useMemo(() => deriveFailureContext(history), [history]);
  const selectedEntry = useMemo(
    () => timeline.find((entry) => entry.sequence === selectedSequence) ?? null,
    [timeline, selectedSequence]
  );

  function selectFromBar(sequence: number, origin?: { x: number }) {
    const container = timelineRef.current;
    setSheetOriginX(origin && container ? origin.x - container.getBoundingClientRect().left : null);
    setSelectedSequence(sequence);
  }

  function selectFromList(sequence: number) {
    setSheetOriginX(null);
    setSelectedSequence(sequence);
  }

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
      {embedded ? (
        <>
          <header className="flex flex-wrap items-center justify-between gap-2">
            <AncestryTrail ancestry={ancestry ?? []} workflowId={workflowId} />
            <div className="flex items-center gap-2">
              <StatusBadge
                isLive={isLive}
                isTerminal={isTerminal}
                terminalOutcome={terminalOutcome}
              />
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
              <Link
                className="text-primary text-xs underline-offset-2 hover:underline"
                data-testid="open-full-view"
                to={workflowDetailHref(workflowId)}
              >
                Open full view
              </Link>
            </div>
          </header>
          {reopenable ? <FailureContextPanel context={failureContext} /> : null}
        </>
      ) : (
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
      )}
      {/* The timeline keeps FULL width; the detail sheet docks BELOW it (PART 2)
          rather than a right-side panel that compresses the chart. */}
      <div className="space-y-3">
        <div className="min-w-0 space-y-3" ref={timelineRef}>
          {mode === 'swimlane' ? (
            <>
              <Scrubber entries={timeline} onScrub={setScrubSeq} scrubSeq={scrubSeq} />
              <Swimlane
                entries={timeline}
                initialExpandedChildren={initialExpandedChildren}
                onSelect={selectFromBar}
                renderChildRun={renderChildRun}
                scrubSeq={scrubSeq}
                selectedSequence={selectedSequence}
              />
            </>
          ) : (
            <EventTimeline
              entries={timeline}
              onSelect={(entry) => selectFromList(entry.sequence)}
              selectedSequence={selectedSequence}
            />
          )}
        </div>
        <DetailSheet
          entry={selectedEntry}
          onClose={() => setSelectedSequence(null)}
          originX={sheetOriginX}
        />
      </div>
      <AttemptNavigator namespace={namespace} timeline={timeline} workflowId={workflowId} />
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
 * The embedded header's ancestor trail: every ancestor links to its own full
 * view (the "zoom out" navigation for deep trees); the current id renders last,
 * unlinked.
 */
function AncestryTrail({
  ancestry,
  workflowId,
}: {
  ancestry: readonly string[];
  workflowId: string;
}) {
  return (
    <nav
      aria-label="Workflow ancestry"
      className="flex min-w-0 flex-wrap items-center gap-1 text-xs"
    >
      {ancestry.map((id) => (
        <Fragment key={id}>
          <Link
            className="truncate font-mono text-muted-foreground hover:text-foreground"
            to={workflowDetailHref(id)}
          >
            {id}
          </Link>
          <span aria-hidden="true" className="text-muted-foreground">
            ›
          </span>
        </Fragment>
      ))}
      <span className="truncate font-mono text-foreground">{workflowId}</span>
    </nav>
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
