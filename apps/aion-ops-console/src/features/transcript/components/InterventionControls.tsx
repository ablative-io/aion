import { useState } from 'react';

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
  const [injectText, setInjectText] = useState('');

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
        {supported.map((primitive) => (
          <li key={primitive.primitive}>
            <PrimitiveControl
              disabled={submitState === 'submitting'}
              injectText={injectText}
              onInjectTextChange={setInjectText}
              onRun={run}
              primitive={primitive.primitive}
            />
          </li>
        ))}
      </ul>
      <OutcomeNotice error={error} outcome={lastOutcome} submitState={submitState} />
    </section>
  );
}

type PrimitiveControlProps = {
  primitive: InterventionPrimitive['primitive'];
  disabled: boolean;
  injectText: string;
  onInjectTextChange: (text: string) => void;
  onRun: (kind: InterventionKind) => void;
};

/** One primitive's control affordance (its inputs + submit button). */
function PrimitiveControl({
  primitive,
  disabled,
  injectText,
  onInjectTextChange,
  onRun,
}: PrimitiveControlProps) {
  if (primitive === 'InjectMessage') {
    return (
      <div className="flex flex-col gap-1.5">
        <textarea
          aria-label="Message to inject"
          className="min-h-16 rounded-lg border border-border bg-surface-default p-2 text-xs"
          onChange={(fieldEvent) => onInjectTextChange(fieldEvent.target.value)}
          placeholder="Steer the agent (interrupt-priority)…"
          value={injectText}
        />
        <Button
          className="h-7 self-start px-3 text-xs"
          disabled={disabled || injectText.trim().length === 0}
          onClick={() =>
            onRun({
              kind: 'InjectMessage',
              text: injectText,
              priority: { priority: 'Interrupt' },
            })
          }
          type="button"
          variant="outline"
        >
          {PRIMITIVE_LABELS.InjectMessage}
        </Button>
      </div>
    );
  }

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
