import { useEffect, useMemo, useRef, useState } from 'react';

import { EmptyState } from '@/components/EmptyState';
import { ErrorState } from '@/components/ErrorState';
import { ChatInputMorph, type EscalationLevel } from '@/components/kit';
import { Badge, Button } from '@/components/ui';
import { TranscriptPanel, useActivityAttempts, useIntervene } from '@/features/transcript';
import { useLiveWorkflowEvents, useWorkflowHistory } from '@/features/workflow-detail';
import type { TranscriptTarget } from '@/lib/api/transcript-stream';
import { ACTION_IDS, useAction } from '@/lib/keybindings';
import type { InterventionOutcome, Namespace, WorkflowId } from '@/types';

import { useAssistantSignal } from '../hooks/useAssistantSignal';
import { ASSISTANT_CONTINUE_SIGNAL, ASSISTANT_END_SIGNAL } from '../lib/contract';
import {
  type AssistantChatMode,
  deriveAssistantMode,
  MODE_PLACEHOLDER,
  MODE_STATUS,
  selectAssistantAttempt,
} from '../lib/mode';

/**
 * One assistant session (v1: steady and stable): the attempt transcript, live,
 * with the kit chat pill docked beneath it. The dock's send verb is MODE-driven
 * (see {@link deriveAssistantMode}): while an attempt is live it intervenes
 * (InjectMessage, interrupt priority — the existing NOI-7 path); when the round
 * has ended and the workflow awaits continuation it signals
 * `assistant_continue { message }`. Ending the session signals `assistant_end`.
 * All state is real — history + socket + live-attempt enumeration; no polling.
 */

export type AssistantSessionViewProps = {
  namespace: Namespace | null;
  workflowId: WorkflowId;
};

export function AssistantSessionView({ namespace, workflowId }: AssistantSessionViewProps) {
  const historyQuery = useWorkflowHistory({ workflowId });
  const live = useLiveWorkflowEvents({
    enabled: historyQuery.isSuccess,
    history: historyQuery.data ?? [],
    workflowId,
  });
  const { attempts, loadState, refresh } = useActivityAttempts({ workflowId, namespace });

  // Attempt liveness changes exactly when lifecycle events land on the socket
  // (activity started/completed, signal received) — re-enumerate then. This is
  // event-driven refresh, not a poll; the initial load is the hook's own.
  const eventCount = live.events.length;
  useEffect(() => {
    if (eventCount > 0) {
      refresh();
    }
  }, [eventCount, refresh]);

  const mode = deriveAssistantMode({
    isTerminal: live.isTerminal,
    attemptsReady: loadState === 'ready',
    liveAttemptCount: attempts.length,
  });
  // The hook returns attempts ascending — the newest is the round in flight.
  const attempt = selectAssistantAttempt(attempts);

  const target = useMemo<TranscriptTarget | null>(() => {
    if (attempt === null || namespace === null) {
      return null;
    }
    return { namespace, workflowId, activityId: attempt.activityId, attempt: attempt.attempt };
  }, [attempt, namespace, workflowId]);

  // Keep the finished round's transcript on screen while the workflow awaits
  // continuation — the live-attempt enumeration drops a finished attempt, but
  // the operator still needs to read what the agent just did.
  const [lastTarget, setLastTarget] = useState<TranscriptTarget | null>(null);
  useEffect(() => {
    if (target !== null) {
      setLastTarget(target);
    }
  }, [target]);
  const displayTarget = target ?? lastTarget;

  const intervene = useIntervene({ namespace });
  const signal = useAssistantSignal({ namespace, workflowId });

  const [chatExpanded, setChatExpanded] = useState(false);
  const dockRef = useRef<HTMLDivElement | null>(null);
  useAction(ACTION_IDS.assistantFocusChat, () => {
    setChatExpanded(true);
    // The morph focuses its textarea on expansion; when already expanded,
    // re-focus the mounted field directly.
    requestAnimationFrame(() => {
      dockRef.current?.querySelector('textarea')?.focus();
    });
  });

  if (namespace === null) {
    return (
      <EmptyState
        description="Select a namespace to open this assistant session."
        title="No namespace selected"
      />
    );
  }
  if (historyQuery.isError) {
    return (
      <ErrorState
        error={historyQuery.error}
        onRetry={() => void historyQuery.refetch()}
        title="Could not load this assistant session"
      />
    );
  }

  const cancelAttempt = () => {
    if (attempt === null) {
      return;
    }
    void intervene
      .submit({
        workflowId,
        activityId: attempt.activityId,
        attempt: attempt.attempt,
        kind: { kind: 'Cancel', reason: 'operator cancelled from assistant panel' },
      })
      .catch(() => {
        // Captured as visible hook state; swallowed here only to avoid an
        // unhandled-rejection warning.
      });
  };

  const handleSubmit = (message: string) => {
    if (mode === 'live' && attempt !== null) {
      void intervene
        .submit({
          workflowId,
          activityId: attempt.activityId,
          attempt: attempt.attempt,
          kind: { kind: 'InjectMessage', text: message, priority: { priority: 'Interrupt' } },
        })
        .catch(() => {});
      return;
    }
    if (mode === 'awaiting') {
      void signal.submit(ASSISTANT_CONTINUE_SIGNAL, { message }).catch(() => {});
    }
  };

  const handleEscalate = (level: EscalationLevel) => {
    // The message IS the interrupt (InjectMessage at interrupt priority), so an
    // empty interrupt press carries no contract action. Shutdown/kill map to
    // the one stop primitive the intervention contract has: Cancel.
    if (level === 'shutdown' || level === 'kill') {
      cancelAttempt();
    }
  };

  const endSession = () => {
    // Contract: `assistant_end {}` — an explicit empty-object payload.
    void signal.submit(ASSISTANT_END_SIGNAL, {}).catch(() => {});
  };

  // Cancel is offered ONLY when the live attempt's harness advertises it.
  const canCancel =
    attempt?.capabilities.supported.some((primitive) => primitive.primitive === 'Cancel') ?? false;

  return (
    <section className="flex flex-col gap-4" data-testid="assistant-session">
      <header className="flex flex-wrap items-center justify-between gap-3">
        <div className="min-w-0 space-y-1">
          <h1 className="font-semibold text-foreground text-lg">Assistant session</h1>
          <p className="font-mono text-muted-foreground text-xs">{workflowId}</p>
        </div>
        <div className="flex items-center gap-2">
          <ModeChip mode={mode} />
          {canCancel ? (
            <Button
              className="h-7 px-3 text-xs"
              disabled={intervene.submitState === 'submitting'}
              onClick={cancelAttempt}
              type="button"
              variant="destructive"
            >
              Cancel attempt
            </Button>
          ) : null}
          <Button
            className="h-7 px-3 text-xs"
            disabled={mode === 'ended' || signal.submitState === 'submitting'}
            onClick={endSession}
            type="button"
            variant="outline"
          >
            End session
          </Button>
        </div>
      </header>

      <TranscriptPanel target={displayTarget} />

      <div ref={dockRef}>
        <SessionDock
          chatExpanded={chatExpanded}
          mode={mode}
          onEscalate={handleEscalate}
          onExpandedChange={setChatExpanded}
          onSubmit={handleSubmit}
          workflowId={workflowId}
        />
      </div>

      <DeliveryNotice
        interveneError={intervene.error}
        outcome={intervene.lastOutcome}
        signalError={signal.error}
      />
    </section>
  );
}

type SessionDockProps = {
  mode: AssistantChatMode;
  workflowId: WorkflowId;
  chatExpanded: boolean;
  onExpandedChange: (expanded: boolean) => void;
  onSubmit: (message: string) => void;
  onEscalate: (level: EscalationLevel) => void;
};

/**
 * The docked chat pill. Only offered when sending has a real meaning ('live' →
 * intervene inject, 'awaiting' → continue signal); the placeholder + status dot
 * make the current verb obvious. Drafts survive the dock unmounting (the kit's
 * module-level draft store), so a mode flip never eats a half-written message.
 */
export function SessionDock({
  mode,
  workflowId,
  chatExpanded,
  onExpandedChange,
  onSubmit,
  onEscalate,
}: SessionDockProps) {
  if (mode === 'connecting') {
    return <p className="text-muted-foreground text-xs">Connecting to session…</p>;
  }
  if (mode === 'ended') {
    return (
      <p className="text-muted-foreground text-xs" data-testid="assistant-session-ended">
        Session ended — start a new one from the assistant list.
      </p>
    );
  }

  return (
    <ChatInputMorph
      capabilities={mode === 'live' ? ['interrupt'] : ['continue']}
      draftKey={`assistant:${workflowId}`}
      expanded={chatExpanded}
      onEscalate={onEscalate}
      onExpandedChange={onExpandedChange}
      onSubmit={onSubmit}
      placeholder={MODE_PLACEHOLDER[mode]}
      status={MODE_STATUS[mode]}
      streaming={mode === 'live'}
    />
  );
}

/** The session-level mode chip (same dot+chip status grammar as StatusBadge). */
const MODE_CHIP: Record<AssistantChatMode, { label: string; className: string }> = {
  live: { label: 'Agent working', className: 'border-live/30 bg-live-glow text-live' },
  awaiting: {
    label: 'Awaiting continuation',
    className: 'border-success/30 bg-success-glow text-success',
  },
  connecting: {
    label: 'Connecting',
    className: 'border-border bg-surface-hover text-muted-foreground',
  },
  ended: { label: 'Ended', className: 'border-border bg-surface-hover text-muted-foreground' },
};

export function ModeChip({ mode }: { mode: AssistantChatMode }) {
  const chip = MODE_CHIP[mode];
  return (
    <Badge className={chip.className} data-mode={mode} variant="outline">
      {chip.label}
    </Badge>
  );
}

type DeliveryNoticeProps = {
  outcome: InterventionOutcome | null;
  interveneError: Error | null;
  signalError: Error | null;
};

/** Honest delivery state under the dock: acks verbatim, errors never swallowed. */
function DeliveryNotice({ outcome, interveneError, signalError }: DeliveryNoticeProps) {
  const error = interveneError ?? signalError;
  if (error !== null) {
    return (
      <p className="text-destructive text-xs" data-testid="assistant-delivery-error" role="alert">
        {error.message}
      </p>
    );
  }
  if (outcome === null || outcome.outcome === 'Applied') {
    // A clean ack needs no banner — the transcript itself shows the effect.
    return null;
  }
  const label =
    outcome.outcome === 'CapabilityNotSupported'
      ? `Not supported: ${outcome.primitive.primitive}`
      : `Too late: ${outcome.detail}`;
  return (
    <Badge
      className="self-start border-warning/40 bg-warning-glow text-warning"
      data-testid="assistant-delivery-outcome"
      variant="outline"
    >
      {label}
    </Badge>
  );
}
