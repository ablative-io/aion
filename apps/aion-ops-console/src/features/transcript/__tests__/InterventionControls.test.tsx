import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import type { AttemptCapabilities } from '@/lib/api';
import type { InterventionPrimitive } from '@/types';

import { InterventionControls } from '../components/InterventionControls';

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
    // Advertised → present.
    expect(markup).toContain('Inject message');
    expect(markup).toContain('Cancel run');
    // NOT advertised → the button must not appear (negative control).
    expect(markup).not.toContain('Pause');
    expect(markup).not.toContain('Update budget');
    expect(markup).not.toContain('Respond to approval');
  });

  test('an empty capability set renders no controls — an observability-only attempt', () => {
    const markup = renderToStaticMarkup(
      <InterventionControls attempt={attempt([])} namespace="default" workflowId={workflowId} />
    );
    expect(markup).toContain('observability-only');
    expect(markup).not.toContain('Inject message');
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
    // The primitives NOT advertised here must not render.
    expect(markup).not.toContain('Inject message');
    expect(markup).not.toContain('Cancel run');
  });
});
