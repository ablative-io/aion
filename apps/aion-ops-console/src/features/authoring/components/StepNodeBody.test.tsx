import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import type { GraphProjection, ProjectionStep } from '../lib/projection-types';
import { StepNodeBody, SubflowGraph } from './StepNodeBody';

function step(name: string, extra: Partial<ProjectionStep> = {}): ProjectionStep {
  return {
    name,
    documentation: '',
    activities: [],
    span: { start: 0, end: 4, line: 1, column: 1 },
    kind: 'plain',
    region: null,
    distribution: null,
    collect: null,
    subflow: null,
    visits: null,
    decision: false,
    waits: false,
    ...extra,
  };
}

const devItemGraph: GraphProjection = {
  steps: [
    step('develop', { documentation: 'Develop one item.' }),
    step('review', { decision: true, visits: '3' }),
  ],
  edges: [
    {
      id: 'fall:develop:review',
      source: 'develop',
      target: 'review',
      kind: 'fall_through',
      label: null,
      back: false,
      visits: null,
    },
    {
      id: 'route:review:develop:when:0',
      source: 'review',
      target: 'develop',
      kind: 'route',
      label: 'when',
      back: true,
      visits: '3',
    },
  ],
  childCalls: [],
};

function render(node: ProjectionStep, expanded: ReadonlySet<string> = new Set()) {
  return renderToStaticMarkup(
    <StepNodeBody
      expanded={expanded}
      onJumpTo={() => undefined}
      onToggleSubflow={() => undefined}
      path={node.name}
      step={node}
    />
  );
}

describe('flow-vocabulary node rendering', () => {
  test('distribute nodes carry the split glyph and the item-in-collection label', () => {
    const html = render(
      step('wave', {
        kind: 'distribute',
        distribution: { binding: 'item', collection: 'state.items' },
      })
    );
    expect(html).toContain('data-kind="distribute"');
    expect(html).toContain('⫴');
    expect(html).toContain('item in state.items');
    expect(html).toContain('distribute');
    expect(html).not.toContain('sequence · in order');
  });

  test('sequence nodes stay visually distinct from distribute', () => {
    const html = render(
      step('ordered', {
        kind: 'sequence',
        distribution: { binding: 'host', collection: 'hosts' },
      })
    );
    expect(html).toContain('data-kind="sequence"');
    expect(html).toContain('⫴');
    expect(html).toContain('host in hosts');
    expect(html).toContain('sequence · in order');
    expect(html).toContain('text-warning');
  });

  test('region members are marked ×N with their opening step', () => {
    const html = render(step('build', { region: 'wave' }));
    expect(html).toContain('data-region="wave"');
    expect(html).toContain('×N');
    expect(html).toContain('Runs once per item of wave');
  });

  test('collect nodes carry the merge glyph and the binding-to-result label with ?', () => {
    const html = render(
      step('gather', {
        kind: 'collect',
        collect: { binding: 'verdict', tolerant: true, result: 'results' },
      })
    );
    expect(html).toContain('data-kind="collect"');
    expect(html).toContain('⫵');
    expect(html).toContain('collect verdict? → results');
  });

  test('strict collect renders without the tolerant marker', () => {
    const html = render(
      step('gather', {
        kind: 'collect',
        collect: { binding: 'note', tolerant: false, result: 'notes' },
      })
    );
    expect(html).toContain('collect note → notes');
    expect(html).not.toContain('note? →');
  });

  test('pure decision steps draw as diamonds', () => {
    const html = render(step('judge', { kind: 'decision', decision: true }));
    expect(html).toContain('data-kind="decision"');
    expect(html).toContain('aria-label="Decision judge"');
    expect(html).toContain('rotate-45');
  });

  test('mixed steps keep the node with a trailing diamond for their arms', () => {
    const html = render(step('fold', { decision: true, visits: '3' }));
    expect(html).toContain('data-kind="plain"');
    expect(html).toContain('aria-label="Step fold"');
    expect(html).toContain('Has outcome arms');
    expect(html).toContain('◇');
  });

  test('subflow-call nodes collapse by default behind an expand toggle', () => {
    const html = render(
      step('build', { kind: 'subflow_call', subflow: { name: 'dev_item', graph: devItemGraph } })
    );
    expect(html).toContain('data-kind="subflow_call"');
    expect(html).toContain('subflow dev_item');
    expect(html).toContain('aria-label="Expand subflow dev_item"');
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain('develop');
  });

  test('expanding a subflow call draws its own graph in place', () => {
    const html = render(
      step('build', { kind: 'subflow_call', subflow: { name: 'dev_item', graph: devItemGraph } }),
      new Set(['build'])
    );
    expect(html).toContain('aria-label="Collapse subflow dev_item"');
    expect(html).toContain('aria-expanded="true"');
    expect(html).toContain('develop');
    expect(html).toContain('review');
    expect(html).toContain('when ×3');
  });

  test('an unexpandable subflow call (invocation cycle guard) offers no toggle', () => {
    const html = render(
      step('build', { kind: 'subflow_call', subflow: { name: 'ping', graph: null } })
    );
    expect(html).toContain('subflow ping');
    expect(html).not.toContain('Expand subflow');
  });

  test('the recursive canvas labels back edges with their visits bound', () => {
    const html = renderToStaticMarkup(
      <SubflowGraph
        expanded={new Set()}
        graph={devItemGraph}
        onJumpTo={() => undefined}
        onToggleSubflow={() => undefined}
        path="build"
      />
    );
    expect(html).toContain('Subflow graph build');
    expect(html).toContain('aria-label="Step develop"');
    expect(html).toContain('aria-label="Step review"');
    expect(html).toContain('when ×3');
  });
});
