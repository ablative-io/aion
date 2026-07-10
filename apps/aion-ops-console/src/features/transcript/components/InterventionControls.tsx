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

import { type InterveneSubmitState, useIntervene } from '../hooks/useIntervene';

/**
 * Capability-gated mid-run intervention controls (NOI-7), split into TWO slots
 * that share ONE {@link useIntervene} instance (one honest submitState / outcome /
 * error stream), owned by {@link useInterventionController}:
 *
 * - {@link InterventionActions} — the non-text primitives (Cancel / Pause /
 *   Update budget / Respond to approval) as inline header buttons. The host places
 *   these AWAY from the composer (the attempt navigator puts them in its section
 *   header) so a destructive Cancel is never a slip of the hand from Send.
 * - {@link InterventionComposer} — the full-width inject-message composer docked
 *   beneath the transcript, plus the outcome/error notice (the operator looks
 *   there after sending). An EMPTY advertised set renders the observability-only
 *   note here, where the composer would be.
 *
 * Gating is unchanged: ONLY the primitives the target attempt's owning worker
 * advertises (`capabilities.supported`) render — an UNadvertised primitive is
 * NEVER rendered, so the operator can only issue commands the harness accepts.
 * Each control POSTs to `/workflows/intervene` and the neutral
 * {@link InterventionOutcome} ack is surfaced HONESTLY: `Applied`,
 * `CapabilityNotSupported`, and `StaleTarget` are distinct visible outcomes (a
 * gated/stale ack is NOT reported as success and a transport error is NOT
 * swallowed).
 */

/** The non-text intervention primitives (everything but the inject composer). */
type ActionPrimitive = Exclude<InterventionPrimitive['primitive'], 'InjectMessage'>;

/** Human labels for the non-text primitives (header action button captions). */
const PRIMITIVE_LABELS: Record<ActionPrimitive, string> = {
  Cancel: 'Cancel run',
  PauseResume: 'Pause',
  UpdateBudget: 'Update budget',
  RespondToApproval: 'Respond to approval',
};

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
 * The shared intervention state both slots render from: ONE submit pipeline, so
 * the header actions and the composer report through the same honest outcome
 * stream (which {@link InterventionComposer} displays).
 */
export type InterventionController = {
  /** The live attempt under control (drives capability gating in both slots). */
  attempt: AttemptCapabilities;
  /**
   * Per-target composer draft key: drafts persist across navigation but never leak
   * between attempts (each `(workflow, activity, attempt)` keeps its own message).
   */
  draftKey: string;
  /** Issue a command; the ack/error surfaces as controller state, never swallowed. */
  run: (kind: InterventionKind) => void;
  submitState: InterveneSubmitState;
  /** The last neutral ack (applied / gated / stale), surfaced verbatim. */
  lastOutcome: InterventionOutcome | null;
  /** The last transport/authorization error (distinct from a first-class ack). */
  error: Error | null;
};

export type UseInterventionControllerOptions = {
  namespace: Namespace | null;
  workflowId: WorkflowId;
  /**
   * The currently-live attempt matching the host's selection, or `null` when the
   * selection is not live — intervention is read-only then and no slot renders.
   */
  attempt: AttemptCapabilities | null;
};

/**
 * Own the single {@link useIntervene} instance for one live attempt target and
 * hand both slots ({@link InterventionActions}, {@link InterventionComposer}) the
 * same {@link InterventionController}. Returns `null` when there is no live
 * attempt to control (the host renders neither slot).
 */
export function useInterventionController(
  options: UseInterventionControllerOptions
): InterventionController | null {
  const { namespace, workflowId, attempt } = options;
  const { submit, submitState, lastOutcome, error } = useIntervene({ namespace });

  if (attempt === null) {
    return null;
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

  return {
    attempt,
    draftKey: `intervene:${workflowId}:${attempt.activityId}:${attempt.attempt}`,
    run,
    submitState,
    lastOutcome,
    error,
  };
}

export type InterventionSlotProps = {
  controller: InterventionController;
};

/**
 * The non-text primitive buttons (header slot). Renders ONLY the advertised
 * non-inject primitives, inline, sized to sit next to the host's header actions
 * (h-7 text-xs); `Cancel` keeps its destructive variant. Renders nothing when no
 * non-inject primitive is advertised. Outcomes report in the composer slot — the
 * shared controller carries them there.
 */
export function InterventionActions({ controller }: InterventionSlotProps) {
  const actions = controller.attempt.capabilities.supported
    .map((primitive) => primitive.primitive)
    .filter((primitive): primitive is ActionPrimitive => primitive !== 'InjectMessage');
  if (actions.length === 0) {
    return null;
  }

  return (
    <ul className="flex items-center gap-2" data-testid="intervention-actions">
      {actions.map((primitive) => (
        <li key={primitive}>
          <Button
            className="h-7 px-3 text-xs"
            disabled={controller.submitState === 'submitting'}
            onClick={() => controller.run(kindFor(primitive))}
            type="button"
            variant={primitive === 'Cancel' ? 'destructive' : 'outline'}
          >
            {PRIMITIVE_LABELS[primitive]}
          </Button>
        </li>
      ))}
    </ul>
  );
}

/**
 * The composer slot (docked full-width beneath the transcript). An advertised
 * `InjectMessage` renders the kit composer ({@link ChatInputMorph}): the docked
 * pill expands into a textarea with the delivery-priority toggle, and submitting
 * issues an `InjectMessage` whose priority is the operator's toggle (see
 * {@link injectKindFor}). The composer is never bespoke-disabled — submission
 * state is surfaced by {@link OutcomeNotice}, which ALWAYS lives in this slot
 * (full-width, directly beneath the composer) because this is where the operator
 * looks after sending; commands issued from the header actions report here too.
 * An EMPTY advertised set renders the observability-only note instead.
 */
export function InterventionComposer({ controller }: InterventionSlotProps) {
  const supported = controller.attempt.capabilities.supported;
  if (supported.length === 0) {
    return (
      <p className="text-muted-foreground text-xs" data-testid="intervention-none">
        This attempt is observability-only — its harness advertises no intervention controls.
      </p>
    );
  }

  const canInject = supported.some((primitive) => primitive.primitive === 'InjectMessage');
  return (
    <div className="flex w-full flex-col gap-2" data-testid="intervention-composer">
      {canInject ? (
        <ChatInputMorph
          capabilities={INJECT_CAPABILITIES}
          draftKey={controller.draftKey}
          onSubmit={(message, priority) => controller.run(injectKindFor(message, priority))}
          placeholder="Steer the agent…"
          status="live"
        />
      ) : null}
      <OutcomeNotice
        error={controller.error}
        outcome={controller.lastOutcome}
        submitState={controller.submitState}
      />
    </div>
  );
}

/**
 * The neutral command payload for the non-text primitives. `InjectMessage` is
 * handled by the composer slot (it needs the text input) and never reaches here.
 * `PauseResume` defaults to pausing and `RespondToApproval` is not issued without
 * a pending `call_id`, so both carry conservative defaults; they light up only
 * when the harness advertises them.
 */
function kindFor(primitive: ActionPrimitive): InterventionKind {
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
  submitState: InterveneSubmitState;
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
