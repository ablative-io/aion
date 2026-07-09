import { ChatInputMorph, type ChatPriority } from '@/components/kit';
import { Badge, Button } from '@/components/ui';
import type { AttemptCapabilities } from '@/lib/api';
import type {
  InterventionKind,
  InterventionOutcome,
  InterventionPrimitive,
  Namespace,
  WorkflowId,
} from '@/types';

import { useIntervene } from '../hooks/useIntervene';

/** Capability chips shown on the inject composer's docked pill (honest wire support). */
const INJECT_CAPABILITIES = ['interrupt', 'queue'] as const;

/**
 * The `InjectMessage` command for a composed draft. The composer's delivery toggle
 * maps onto the wire {@link InjectPriority} union: `interrupt` acts now (`Interrupt`),
 * `queued` queues the turn (`Normal` — the wire's "queue the turn" variant). Both are
 * genuinely supported, so the pill advertises `interrupt` and `queue` honestly.
 */
export function injectKindFor(text: string, priority: ChatPriority): InterventionKind {
  return {
    kind: 'InjectMessage',
    text,
    priority: { priority: priority === 'interrupt' ? 'Interrupt' : 'Normal' },
  };
}

/**
 * Capability-gated mid-run intervention controls (NOI-7).
 *
 * Renders ONLY the primitives the target attempt's owning worker advertises
 * (`capabilities.supported`) — an UNadvertised primitive is NEVER rendered, so the
 * operator can only issue commands the harness accepts. Each control POSTs to
 * `/workflows/intervene` and surfaces the neutral {@link InterventionOutcome} ack
 * HONESTLY: `Applied`, `CapabilityNotSupported`, and `StaleTarget` are all shown as
 * distinct visible outcomes (a gated/stale ack is NOT reported as success and a
 * transport error is NOT swallowed). An empty advertised set renders no controls —
 * an observability-only attempt.
 */

export type InterventionControlsProps = {
  namespace: Namespace | null;
  workflowId: WorkflowId;
  attempt: AttemptCapabilities;
};

/** Human labels for each neutral primitive (control button captions). */
const PRIMITIVE_LABELS: Record<InterventionPrimitive['primitive'], string> = {
  InjectMessage: 'Inject message',
  Cancel: 'Cancel run',
  PauseResume: 'Pause',
  UpdateBudget: 'Update budget',
  RespondToApproval: 'Respond to approval',
};

/** The data-bound controls: owns the intervene submit for one attempt target. */
export function InterventionControls({
  namespace,
  workflowId,
  attempt,
}: InterventionControlsProps) {
  const { submit, submitState, lastOutcome, error } = useIntervene({ namespace });

  // Per-target draft key: drafts persist across navigation but never leak between
  // attempts (each `(workflow, activity, attempt)` keeps its own composed message).
  const draftKey = `intervene:${workflowId}:${attempt.activityId}:${attempt.attempt}`;

  const supported = attempt.capabilities.supported;
  if (supported.length === 0) {
    return (
      <p className="text-muted-foreground text-xs" data-testid="intervention-none">
        This attempt is observability-only — its harness advertises no intervention controls.
      </p>
    );
  }

  const run = (kind: InterventionKind): void => {
    void submit({
      workflowId,
      activityId: attempt.activityId,
      attempt: attempt.attempt,
      kind,
    }).catch(() => {
      // The error is already captured as visible state by the hook; swallow the
      // rejection here only to avoid an unhandled-promise warning.
    });
  };

  return (
    <section className="flex flex-col gap-2" data-testid="intervention-controls">
      <h3 className="font-medium text-foreground text-xs">Intervene</h3>
      <ul className="flex flex-col gap-2">
        {supported.map((primitive) =>
          primitive.primitive === 'InjectMessage' ? (
            <li key={primitive.primitive}>
              <InjectMessageControl draftKey={draftKey} onRun={run} />
            </li>
          ) : (
            <li key={primitive.primitive}>
              <PrimitiveControl
                disabled={submitState === 'submitting'}
                onRun={run}
                primitive={primitive.primitive}
              />
            </li>
          )
        )}
      </ul>
      <OutcomeNotice error={error} outcome={lastOutcome} submitState={submitState} />
    </section>
  );
}

type InjectMessageControlProps = {
  draftKey: string;
  onRun: (kind: InterventionKind) => void;
};

/**
 * The inject-message affordance: the kit composer ({@link ChatInputMorph}) docked as
 * a slim pill that expands into a textarea with a delivery-priority toggle. Submitting
 * issues an `InjectMessage` command whose priority is the operator's toggle (see
 * {@link injectKindFor}). No escalation machine is wired here — Cancel is a separate
 * gated control — and the composer is never bespoke-disabled: submission state is
 * surfaced by {@link OutcomeNotice}, exactly as the assistant page does it.
 */
function InjectMessageControl({ draftKey, onRun }: InjectMessageControlProps) {
  return (
    <ChatInputMorph
      capabilities={INJECT_CAPABILITIES}
      draftKey={draftKey}
      onSubmit={(message, priority) => onRun(injectKindFor(message, priority))}
      placeholder="Steer the agent…"
      status="live"
    />
  );
}

type PrimitiveControlProps = {
  primitive: Exclude<InterventionPrimitive['primitive'], 'InjectMessage'>;
  disabled: boolean;
  onRun: (kind: InterventionKind) => void;
};

/** One non-text primitive's control affordance (its submit button). */
function PrimitiveControl({ primitive, disabled, onRun }: PrimitiveControlProps) {
  return (
    <Button
      className="h-7 px-3 text-xs"
      disabled={disabled}
      onClick={() => onRun(kindFor(primitive))}
      type="button"
      variant={primitive === 'Cancel' ? 'destructive' : 'outline'}
    >
      {PRIMITIVE_LABELS[primitive]}
    </Button>
  );
}

/**
 * The neutral command payload for the non-text primitives. `InjectMessage` is
 * handled by its dedicated control (it needs the text input) and never reaches
 * here. `PauseResume` defaults to pausing and `RespondToApproval` is not issued
 * without a pending `call_id`, so both carry conservative defaults; they light up
 * only when the harness advertises them.
 */
function kindFor(
  primitive: Exclude<InterventionPrimitive['primitive'], 'InjectMessage'>
): InterventionKind {
  switch (primitive) {
    case 'Cancel':
      return { kind: 'Cancel', reason: 'operator cancelled from ops console' };
    case 'PauseResume':
      return { kind: 'PauseResume', paused: true };
    case 'UpdateBudget':
      return { kind: 'UpdateBudget', max_tokens: null, max_turns: null };
    case 'RespondToApproval':
      return {
        kind: 'RespondToApproval',
        call_id: '',
        decision: { decision: 'Approve' },
        note: null,
      };
  }
}

type OutcomeNoticeProps = {
  submitState: ReturnType<typeof useIntervene>['submitState'];
  outcome: InterventionOutcome | null;
  error: Error | null;
};

/** Surface the last ack / error as distinct, honest visible state. */
function OutcomeNotice({ submitState, outcome, error }: OutcomeNoticeProps) {
  if (submitState === 'error' && error !== null) {
    return (
      <p className="text-destructive text-xs" data-testid="intervention-error" role="alert">
        {error.message}
      </p>
    );
  }
  if (outcome === null) {
    return null;
  }

  const { label, tone } = describeOutcome(outcome);
  return (
    <Badge
      className={tone}
      data-outcome={outcome.outcome}
      data-testid="intervention-outcome"
      variant="outline"
    >
      {label}
    </Badge>
  );
}

/** Map each neutral outcome to an honest label + tone (never conflates classes). */
function describeOutcome(outcome: InterventionOutcome): { label: string; tone: string } {
  switch (outcome.outcome) {
    case 'Applied':
      return {
        label: 'Applied',
        tone: 'border-success/40 bg-success-glow text-success',
      };
    case 'CapabilityNotSupported':
      return {
        label: `Not supported: ${outcome.primitive.primitive}`,
        tone: 'border-warning/40 bg-warning-glow text-warning',
      };
    case 'StaleTarget':
      return {
        label: `Too late: ${outcome.detail}`,
        tone: 'border-warning/40 bg-warning-glow text-warning',
      };
  }
}
