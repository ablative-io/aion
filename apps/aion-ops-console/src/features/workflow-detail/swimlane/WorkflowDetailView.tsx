import { useMemo, useState } from 'react';

import { EmptyState } from '@/components/EmptyState';
import { ErrorState } from '@/components/ErrorState';
import { LoadingSkeleton } from '@/components/LoadingSkeleton';
import { Badge, Button } from '@/components/ui';
import { AttemptTranscriptView } from '@/features/transcript';
import { cn } from '@/lib/utils';
import type { Event } from '@/types';

import { DetailPanel } from '../components/DetailPanel';
import { EventTimeline } from '../components/EventTimeline';
import { useLiveWorkflowEvents } from '../hooks/useLiveWorkflowEvents';
import { useWorkflowHistory } from '../hooks/useWorkflowHistory';
import type { projectTimeline } from '../lib/timeline';
import { deriveFailureContext, ReopenDiff } from '../reopen';
import type { LifecycleOutcome, WorkflowDetailProps } from '../types';
import { Scrubber } from './Scrubber';
import { Swimlane } from './Swimlane';

type ViewMode = 'swimlane' | 'list';

/**
 * Detail view wrapper that adds a List ⇄ Swimlane toggle on top of S3's data
 * (VISION §4.1; PLAN S4). Reuses S3's history + live hooks, projection engine,
 * EventTimeline, and DetailPanel verbatim — it does NOT edit any S3 file and adds
 * no new data binding. The swimlane consumes the same projected entries; selection
 * + DetailPanel are shared across both modes so a stranger reads the same workflow
 * two ways. S0's `/workflows/:id` route renders this wrapper.
 */
function WorkflowDetailView({ workflowId, namespace }: WorkflowDetailProps) {
  const historyQuery = useWorkflowHistory({ workflowId });
  const live = useLiveWorkflowEvents({
    enabled: historyQuery.isSuccess,
    history: historyQuery.data ?? [],
    workflowId,
  });

  return (
    <WorkflowDetailViewContent
      error={historyQuery.error}
      history={live.events}
      isError={historyQuery.isError}
      isLive={!live.isTerminal && namespace !== null}
      isLoading={historyQuery.isLoading || historyQuery.isPending}
      isTerminal={live.isTerminal}
      namespace={namespace}
      onRetry={() => void historyQuery.refetch()}
      terminalOutcome={live.terminalOutcome}
      timeline={live.timeline}
      workflowId={workflowId}
    />
  );
}

type ContentProps = WorkflowDetailProps & {
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
  /** Initial mode, overridable for tests. */
  initialMode?: ViewMode;
  /** Open the reopen-diff panel on first render (tests only). */
  initialReopenOpen?: boolean;
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
}: ContentProps) {
  const [mode, setMode] = useState<ViewMode>(initialMode);
  const [selectedSequence, setSelectedSequence] = useState<number | null>(null);
  const [scrubSeq, setScrubSeq] = useState<number | null>(null);
  const [reopenOpen, setReopenOpen] = useState(initialReopenOpen);
  const reopenable = isReopenable(isTerminal, terminalOutcome);
  const failureContext = useMemo(() => deriveFailureContext(history), [history]);
  const selectedEntry = useMemo(
    () => timeline.find((entry) => entry.sequence === selectedSequence) ?? null,
    [timeline, selectedSequence]
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
      <div className="flex flex-col gap-4 lg:flex-row lg:items-start">
        <div className="min-w-0 flex-1 space-y-3">
          {mode === 'swimlane' ? (
            <>
              <Scrubber entries={timeline} onScrub={setScrubSeq} scrubSeq={scrubSeq} />
              <Swimlane
                entries={timeline}
                onSelect={setSelectedSequence}
                scrubSeq={scrubSeq}
                selectedSequence={selectedSequence}
              />
            </>
          ) : (
            <EventTimeline
              entries={timeline}
              onSelect={(entry) => setSelectedSequence(entry.sequence)}
              selectedSequence={selectedSequence}
            />
          )}
        </div>
        <DetailPanel entry={selectedEntry} onClose={() => setSelectedSequence(null)} />
      </div>
      <AttemptTranscriptView namespace={namespace} workflowId={workflowId} />
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

export { WorkflowDetailView, WorkflowDetailViewContent };
