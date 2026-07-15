import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import { parallelEdgeOffsets } from '../lib/canvas-layout';
import { createAuthoringFacade } from '../lib/facade';
import type { GraphProjection, ProjectionStep } from '../lib/projection-types';
import { ParallelCanvasEdgeVisual } from './CanvasEdge';
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
    substeps: [],
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

const parallelRoutes: GraphProjection = {
  steps: [step('decide', { decision: true }), step('finish')],
  edges: [
    {
      id: 'route:decide:finish:when:0',
      source: 'decide',
      target: 'finish',
      kind: 'route',
      label: 'when',
      back: false,
      visits: null,
    },
    {
      id: 'route:decide:finish:otherwise:1',
      source: 'decide',
      target: 'finish',
      kind: 'route',
      label: 'otherwise',
      back: false,
      visits: null,
    },
    {
      id: 'route:decide:finish:failure:2',
      source: 'decide',
      target: 'finish',
      kind: 'route',
      label: 'failure',
      back: false,
      visits: null,
    },
  ],
  childCalls: [],
};

function renderedAttribute(
  html: string,
  element: 'path' | 'text',
  edgeId: string,
  attribute: string
): string | null {
  const identity = element === 'path' ? 'data-edge-id' : 'data-label-for';
  const marker = `${identity}="${edgeId}"`;
  const markerIndex = html.indexOf(marker);
  if (markerIndex < 0) return null;
  const tagStart = html.lastIndexOf(`<${element}`, markerIndex);
  const tagEnd = html.indexOf('>', markerIndex);
  if (tagStart < 0 || tagEnd < 0) return null;
  const tag = html.slice(tagStart, tagEnd);
  const match = tag.match(new RegExp(`${attribute}="([^"]+)"`));
  return match?.[1] ?? null;
}

function edgeGeometry(html: string) {
  const paths = parallelRoutes.edges.map((edge) => renderedAttribute(html, 'path', edge.id, 'd'));
  const labels = parallelRoutes.edges.map((edge) => {
    const x = renderedAttribute(html, 'text', edge.id, 'x');
    const y = renderedAttribute(html, 'text', edge.id, 'y');
    return `${x},${y}`;
  });
  return { paths, labels };
}

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

  test('parses and expands a parent-owned sibling substep graph', async () => {
    const span = { start: 0, end: 4, line: 1, column: 1 };
    const wireStep = (name: string, extra: Record<string, unknown> = {}) => ({
      name,
      documentation: '',
      activities: [],
      span,
      kind: 'plain',
      region: null,
      distribution: null,
      collect: null,
      subflow: null,
      substeps: [],
      visits: null,
      decision: false,
      waits: false,
      ...extra,
    });
    const scopedGraph = {
      steps: [wireStep('fetch_batch'), wireStep('scrub')],
      edges: [
        {
          id: 'route:fetch_batch:scrub:when:0',
          source: 'fetch_batch',
          target: 'scrub',
          kind: 'route',
          label: 'when',
          back: false,
          visits: null,
        },
      ],
      child_calls: [],
    };
    const facade = createAuthoringFacade(async () =>
      Response.json({
        ok: true,
        deploys_green: true,
        steps: 3,
        diagnostics: [],
        semantic: {
          entries: [],
          graph: {
            steps: [
              wireStep('prepare', {
                substeps: [
                  { scope: 'body', index: 0, graph: scopedGraph },
                  { scope: 'failure', index: 0, graph: scopedGraph },
                  { scope: 'fork', index: 0, graph: scopedGraph },
                  { scope: 'loop', index: 1, graph: scopedGraph },
                ],
              }),
            ],
            edges: [],
            child_calls: [],
          },
          studio: { builtins: [], types: [], workers: [] },
        },
      })
    );
    const result = await facade.check('workflow prepare_dataset');
    const prepare = result.semantic?.graph.steps[0];
    expect(prepare?.substeps.map((scoped) => [scoped.scope, scoped.index])).toEqual([
      ['body', 0],
      ['failure', 0],
      ['fork', 0],
      ['loop', 1],
    ]);
    expect(prepare?.substeps[1]?.graph.steps.map((nested) => nested.name)).toEqual([
      'fetch_batch',
      'scrub',
    ]);
    if (prepare === undefined) return;
    const html = render(prepare, new Set(['prepare/@failure-0']));
    expect(html).toContain('Body substeps');
    expect(html).toContain('aria-label="Collapse Failure substeps of prepare"');
    expect(html).toContain('Fork 1 substeps');
    expect(html).toContain('Loop 2 substeps');
    expect(html).toContain('aria-label="Failure substeps graph of prepare"');
    expect(html).toContain('aria-label="Step fetch_batch"');
    expect(html).toContain('aria-label="Step scrub"');
    expect(html).toContain('data-edge-id="route:fetch_batch:scrub:when:0"');
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

  test('embedded outcome and failure siblings render distinct paths and labels', () => {
    const html = renderToStaticMarkup(
      <SubflowGraph
        expanded={new Set()}
        graph={parallelRoutes}
        onJumpTo={() => undefined}
        onToggleSubflow={() => undefined}
        path="decision"
      />
    );
    const geometry = edgeGeometry(html);
    expect(geometry.paths.every((path) => path !== null)).toBe(true);
    expect(new Set(geometry.paths).size).toBe(3);
    expect(new Set(geometry.labels).size).toBe(3);
  });

  test('root outcome and failure siblings render distinct paths and labels', () => {
    const offsets = parallelEdgeOffsets(parallelRoutes.edges);
    const html = renderToStaticMarkup(
      <svg>
        <title>Root parallel routes</title>
        {parallelRoutes.edges.map((edge) => (
          <ParallelCanvasEdgeVisual
            back={false}
            graphLeft={0}
            graphRight={200}
            id={edge.id}
            key={edge.id}
            label={edge.label}
            selfLoop={false}
            siblingOffset={offsets.get(edge.id) ?? 0}
            sourceX={100}
            sourceY={100}
            targetX={100}
            targetY={300}
          />
        ))}
      </svg>
    );
    const geometry = edgeGeometry(html);
    expect(geometry.paths.every((path) => path !== null)).toBe(true);
    expect(new Set(geometry.paths).size).toBe(3);
    expect(new Set(geometry.labels).size).toBe(3);
  });
});
