import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import type { AttemptCapabilities } from '@/lib/api';
import type { InterventionPrimitive } from '@/types';

import {
  InterventionActions,
  InterventionComposer,
  type InterventionController,
  injectKindFor,
  useInterventionController,
} from '../components/InterventionControls';

const workflowId = '00000000-0000-0000-0000-000000000001';

function attempt(primitives: InterventionPrimitive['primitive'][]): AttemptCapabilities {
  return {
    activityId: 3,
    attempt: 1,
    capabilities: { supported: primitives.map((primitive) => ({ primitive })) },
  };
}

/** A fabricated controller for exercising the presentational slots directly. */
function controllerFor(
  capabilities: AttemptCapabilities,
  overrides: Partial<InterventionController> = {}
): InterventionController {
  return {
    attempt: capabilities,
    draftKey: `intervene:${workflowId}:3:1`,
    run: () => {},
    submitState: 'idle',
    lastOutcome: null,
    error: null,
    ...overrides,
  };
}

/**
 * Mirrors the AttemptNavigator wiring: one shared controller feeding both slots
 * (header actions + composer), or nothing at all when the attempt is not live.
 */
function Slots({ liveAttempt }: { liveAttempt: AttemptCapabilities | null }) {
  const controller = useInterventionController({
    namespace: 'default',
    workflowId,
    attempt: liveAttempt,
  });
  if (controller === null) {
    return <p data-testid="read-only">read-only</p>;
  }
  return (
    <div>
      <header data-testid="actions-slot">
        <InterventionActions controller={controller} />
      </header>
      <div data-testid="composer-slot">
        <InterventionComposer controller={controller} />
      </div>
    </div>
  );
}

describe('intervention slots — capability gating', () => {
  test('renders ONLY the advertised primitives; an unadvertised one is NEVER rendered', () => {
    const markup = renderToStaticMarkup(
      <Slots liveAttempt={attempt(['InjectMessage', 'Cancel'])} />
    );
    // InjectMessage → the kit composer (docked pill) in the composer slot.
    expect(markup).toContain('data-slot="chat-input"');
    expect(markup).toContain('Steer the agent…');
    // Cancel → a header action with the destructive variant.
    expect(markup).toContain('Cancel run');
    expect(markup).toContain('data-variant="destructive"');
    // NOT advertised → the control must not appear (negative control).
    expect(markup).not.toContain('Pause');
    expect(markup).not.toContain('Update budget');
    expect(markup).not.toContain('Respond to approval');
  });

  test('the destructive actions render in the HEADER slot, away from the composer', () => {
    const markup = renderToStaticMarkup(
      <Slots liveAttempt={attempt(['InjectMessage', 'Cancel'])} />
    );
    // The action buttons live inside the actions slot…
    const actionsSlot = markup.slice(
      markup.indexOf('data-testid="actions-slot"'),
      markup.indexOf('data-testid="composer-slot"')
    );
    expect(actionsSlot).toContain('data-testid="intervention-actions"');
    expect(actionsSlot).toContain('Cancel run');
    // …and the composer slot carries NO action button, only the composer.
    const composerSlot = markup.slice(markup.indexOf('data-testid="composer-slot"'));
    expect(composerSlot).toContain('data-testid="intervention-composer"');
    expect(composerSlot).toContain('data-slot="chat-input"');
    expect(composerSlot).not.toContain('Cancel run');
  });

  test('inject-only: the composer renders and the actions slot renders nothing', () => {
    const markup = renderToStaticMarkup(<Slots liveAttempt={attempt(['InjectMessage'])} />);
    expect(markup).toContain('data-slot="chat-input"');
    expect(markup).not.toContain('data-testid="intervention-actions"');
    // The composer advertises exactly its honest wire capabilities + live status.
    expect(markup).toContain('interrupt');
    expect(markup).toContain('queue');
    expect(markup).toContain('data-status="live"');
  });

  test('an empty capability set renders the observability-only note where the composer would be', () => {
    const markup = renderToStaticMarkup(<Slots liveAttempt={attempt([])} />);
    const composerSlot = markup.slice(markup.indexOf('data-testid="composer-slot"'));
    expect(composerSlot).toContain('observability-only');
    expect(markup).not.toContain('data-slot="chat-input"');
    expect(markup).not.toContain('data-testid="intervention-actions"');
    expect(markup).not.toContain('Cancel run');
  });

  test('actions-only: the advertised buttons light up; no composer, but the outcome surface stays', () => {
    const markup = renderToStaticMarkup(
      <Slots liveAttempt={attempt(['PauseResume', 'UpdateBudget'])} />
    );
    expect(markup).toContain('Pause');
    expect(markup).toContain('Update budget');
    expect(markup).not.toContain('data-slot="chat-input"');
    expect(markup).not.toContain('Cancel run');
    // The composer slot still exists as the shared outcome surface.
    expect(markup).toContain('data-testid="intervention-composer"');
  });

  test('no live attempt → the controller is null and neither slot renders', () => {
    const markup = renderToStaticMarkup(<Slots liveAttempt={null} />);
    expect(markup).toContain('data-testid="read-only"');
    expect(markup).not.toContain('data-testid="intervention-actions"');
    expect(markup).not.toContain('data-testid="intervention-composer"');
    expect(markup).not.toContain('data-slot="chat-input"');
  });
});

describe('intervention outcome/error — reported in the composer slot, verbatim', () => {
  test('an Applied ack renders as a distinct visible outcome next to the composer', () => {
    const controller = controllerFor(attempt(['InjectMessage', 'Cancel']), {
      submitState: 'settled',
      lastOutcome: { outcome: 'Applied' },
    });
    const markup = renderToStaticMarkup(<InterventionComposer controller={controller} />);
    expect(markup).toContain('data-testid="intervention-outcome"');
    expect(markup).toContain('data-outcome="Applied"');
    expect(markup).toContain('Applied');
  });

  test('a gated ack is shown honestly, never conflated with success', () => {
    const controller = controllerFor(attempt(['Cancel']), {
      submitState: 'settled',
      lastOutcome: { outcome: 'CapabilityNotSupported', primitive: { primitive: 'Cancel' } },
    });
    const markup = renderToStaticMarkup(<InterventionComposer controller={controller} />);
    expect(markup).toContain('data-outcome="CapabilityNotSupported"');
    expect(markup).toContain('Not supported: Cancel');
  });

  test('a stale-target ack is shown honestly with its detail', () => {
    const controller = controllerFor(attempt(['Cancel']), {
      submitState: 'settled',
      lastOutcome: { outcome: 'StaleTarget', detail: 'attempt 1 superseded' },
    });
    const markup = renderToStaticMarkup(<InterventionComposer controller={controller} />);
    expect(markup).toContain('data-outcome="StaleTarget"');
    expect(markup).toContain('Too late: attempt 1 superseded');
  });

  test('a transport error surfaces as visible alert state, not swallowed', () => {
    const controller = controllerFor(attempt(['InjectMessage']), {
      submitState: 'error',
      error: new Error('intervene endpoint unreachable'),
    });
    const markup = renderToStaticMarkup(<InterventionComposer controller={controller} />);
    expect(markup).toContain('data-testid="intervention-error"');
    expect(markup).toContain('intervene endpoint unreachable');
  });

  test('while submitting, the header action buttons are disabled', () => {
    const controller = controllerFor(attempt(['Cancel', 'PauseResume']), {
      submitState: 'submitting',
    });
    const markup = renderToStaticMarkup(<InterventionActions controller={controller} />);
    expect(markup).toContain('disabled');
  });
});

describe('injectKindFor — composer priority maps onto the wire InjectPriority union', () => {
  test('the interrupt toggle acts now (Interrupt)', () => {
    expect(injectKindFor('steer now', 'interrupt')).toEqual({
      kind: 'InjectMessage',
      text: 'steer now',
      priority: { priority: 'Interrupt' },
    });
  });

  test('the queued toggle queues the turn (Normal — the wire’s queue variant)', () => {
    expect(injectKindFor('follow up', 'queued')).toEqual({
      kind: 'InjectMessage',
      text: 'follow up',
      priority: { priority: 'Normal' },
    });
  });
});

describe('useInterventionController — per-target draft key', () => {
  test('the draft key is scoped to (workflow, activity, attempt) so drafts never leak', () => {
    const observed: { draftKey: string | null } = { draftKey: null };
    function Probe() {
      const controller = useInterventionController({
        namespace: 'default',
        workflowId,
        attempt: attempt(['InjectMessage']),
      });
      observed.draftKey = controller?.draftKey ?? null;
      return null;
    }
    renderToStaticMarkup(<Probe />);
    expect(observed.draftKey).toBe(`intervene:${workflowId}:3:1`);
  });
});
