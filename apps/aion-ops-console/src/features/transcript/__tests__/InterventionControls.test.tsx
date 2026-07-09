import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import type { AttemptCapabilities } from '@/lib/api';
import type { InterventionPrimitive } from '@/types';

import { InterventionControls, injectKindFor } from '../components/InterventionControls';

const workflowId = '00000000-0000-0000-0000-000000000001';

function attempt(primitives: InterventionPrimitive['primitive'][]): AttemptCapabilities {
  return {
    activityId: 3,
    attempt: 1,
    capabilities: { supported: primitives.map((primitive) => ({ primitive })) },
  };
}

describe('InterventionControls capability gating', () => {
  test('renders ONLY the advertised primitives; an unadvertised one is NEVER rendered', () => {
    const markup = renderToStaticMarkup(
      <InterventionControls
        attempt={attempt(['InjectMessage', 'Cancel'])}
        namespace="default"
        workflowId={workflowId}
      />
    );
    // InjectMessage → the kit composer (docked pill), not a bare textarea+button.
    expect(markup).toContain('data-slot="chat-input"');
    expect(markup).toContain('Steer the agent…');
    expect(markup).toContain('Cancel run');
    // NOT advertised → the control must not appear (negative control).
    expect(markup).not.toContain('Pause');
    expect(markup).not.toContain('Update budget');
    expect(markup).not.toContain('Respond to approval');
  });

  test('the inject composer advertises exactly its honest wire capabilities', () => {
    const markup = renderToStaticMarkup(
      <InterventionControls
        attempt={attempt(['InjectMessage'])}
        namespace="default"
        workflowId={workflowId}
      />
    );
    // Both delivery modes are genuinely supported on the wire, so both chips show.
    expect(markup).toContain('interrupt');
    expect(markup).toContain('queue');
    // A live attempt drives the composer's live status dot.
    expect(markup).toContain('data-status="live"');
  });

  test('an empty capability set renders no controls — an observability-only attempt', () => {
    const markup = renderToStaticMarkup(
      <InterventionControls attempt={attempt([])} namespace="default" workflowId={workflowId} />
    );
    expect(markup).toContain('observability-only');
    expect(markup).not.toContain('data-slot="chat-input"');
    expect(markup).not.toContain('Cancel run');
  });

  test('a distinct set lights up exactly its advertised primitives', () => {
    const markup = renderToStaticMarkup(
      <InterventionControls
        attempt={attempt(['PauseResume', 'UpdateBudget'])}
        namespace="default"
        workflowId={workflowId}
      />
    );
    expect(markup).toContain('Pause');
    expect(markup).toContain('Update budget');
    // The primitives NOT advertised here must not render — including the composer.
    expect(markup).not.toContain('data-slot="chat-input"');
    expect(markup).not.toContain('Cancel run');
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
