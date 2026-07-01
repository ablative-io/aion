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
import { ReopenDiff } from '../reopen';
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

/** A reopen is only meaningful once the engine has recorded a WorkflowReopened event. */
function hasWorkflowReopened(history: readonly Event[]): boolean {
  return history.some((event) => event.type === 'WorkflowReopened');
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
  const reopenable = useMemo(() => hasWorkflowReopened(history), [history]);
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
        <p className="text-[var(--text-muted)] text-sm">Namespace {namespace}</p>
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="flex flex-wrap items-center gap-3">
            <h1 className="font-semibold text-2xl text-[var(--text-primary)]">
              Workflow {workflowId}
            </h1>
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
                onClick={() => setReopenOpen(true)}
                type="button"
                variant="outline"
              >
                Reopen diff
              </Button>
            ) : null}
            <ViewToggle mode={mode} onChange={setMode} />
          </div>
        </div>
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
          onClose={() => setReopenOpen(false)}
          workflowId={workflowId}
        />
      ) : null}
    </section>
  );
}

function ViewToggle({ mode, onChange }: { mode: ViewMode; onChange: (mode: ViewMode) => void }) {
  return (
    <fieldset
      aria-label="Detail view mode"
      className="inline-flex rounded-lg border border-[var(--border-default)] p-0.5"
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
      className={cn('h-7 px-3 text-xs', active && 'bg-[var(--surface-hover)]')}
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
