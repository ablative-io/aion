import { describe, expect, test } from 'bun:test';

import type { GraphProjection } from './projection-types';
import { activeStepForActivity, hasRevisionDrift } from './run-mode';

const graph: GraphProjection = {
  steps: [
    {
      name: 'prepare',
      documentation: 'Prepare the order',
      activities: ['prepare_order'],
      markers: { forked: false, looped: false, waits: false },
      span: { start: 0, end: 10, line: 1, column: 1 },
    },
  ],
  edges: [],
  childCalls: [],
};

describe('run-mode revision binding', () => {
  test('reports drift only when the editor differs from the deployed revision', () => {
    expect(hasRevisionDrift('same', 'same')).toBe(false);
    expect(hasRevisionDrift('edited', 'deployed')).toBe(true);
  });

  test('maps live activity events onto the deployed graph step', () => {
    expect(activeStepForActivity(graph, 'prepare_order')).toBe('prepare');
    expect(activeStepForActivity(graph, 'other')).toBeNull();
    expect(activeStepForActivity(graph, null)).toBeNull();
  });
});
