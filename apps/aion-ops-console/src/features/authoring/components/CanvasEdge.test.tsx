import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import { layoutEmbedded } from '../lib/canvas-layout';
import type { GraphProjection, ProjectionStep } from '../lib/projection-types';
import { ParallelCanvasEdgeVisual } from './CanvasEdge';
import { SubflowGraph } from './StepNodeBody';

function step(name: string): ProjectionStep {
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
  };
}

const selfLoopId = 'route:repeat:repeat:otherwise:0';
const selfLoopGraph: GraphProjection = {
  steps: [step('repeat')],
  edges: [
    {
      id: selfLoopId,
      source: 'repeat',
      target: 'repeat',
      kind: 'route',
      label: 'otherwise',
      back: true,
      visits: '3',
    },
  ],
  childCalls: [],
};

const backEdgeId = 'route:later:earlier:when:0';
const backEdgeGraph: GraphProjection = {
  steps: [step('earlier'), step('later')],
  edges: [
    {
      id: 'fall:earlier:later',
      source: 'earlier',
      target: 'later',
      kind: 'fall_through',
      label: null,
      back: false,
      visits: null,
    },
    {
      id: backEdgeId,
      source: 'later',
      target: 'earlier',
      kind: 'route',
      label: 'when',
      back: true,
      visits: '3',
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
  const markerIndex = html.indexOf(`${identity}="${edgeId}"`);
  if (markerIndex < 0) return null;
  const tagStart = html.lastIndexOf(`<${element}`, markerIndex);
  const tagEnd = html.indexOf('>', markerIndex);
  if (tagStart < 0 || tagEnd < 0) return null;
  const tag = html.slice(tagStart, tagEnd);
  return tag.match(new RegExp(`${attribute}="([^"]+)"`))?.[1] ?? null;
}

function pathCoordinates(html: string, edgeId: string): number[] {
  const path = renderedAttribute(html, 'path', edgeId, 'd');
  return path?.match(/-?\d+(?:\.\d+)?/g)?.map(Number) ?? [];
}

function laneXs(html: string, edgeId: string): number[] {
  const coordinates = pathCoordinates(html, edgeId);
  return [coordinates[4], coordinates[6], coordinates[8], coordinates[10]].filter(
    (coordinate): coordinate is number => coordinate !== undefined
  );
}

function exteriorYs(html: string, edgeId: string): number[] {
  const coordinates = pathCoordinates(html, edgeId);
  return [coordinates[3], coordinates[9]].filter(
    (coordinate): coordinate is number => coordinate !== undefined
  );
}

function labelX(html: string, edgeId: string): number {
  return Number(renderedAttribute(html, 'text', edgeId, 'x'));
}

function expectExteriorPath(
  html: string,
  edgeId: string,
  side: 'left' | 'right',
  lateralBoundary: number,
  sourceBottom: number,
  targetTop: number
) {
  const lanes = laneXs(html, edgeId);
  expect(lanes).toHaveLength(4);
  expect(lanes.every((x) => (side === 'right' ? x > lateralBoundary : x < lateralBoundary))).toBe(
    true
  );
  const outer = exteriorYs(html, edgeId);
  expect(outer).toHaveLength(2);
  expect(Number(outer[0])).toBeGreaterThan(sourceBottom);
  expect(Number(outer[1])).toBeLessThan(targetTop);
}

function renderEmbedded(graph: GraphProjection, path: string): string {
  return renderToStaticMarkup(
    <SubflowGraph
      expanded={new Set()}
      graph={graph}
      onJumpTo={() => undefined}
      onToggleSubflow={() => undefined}
      path={path}
    />
  );
}

describe('back-edge and self-loop clearance', () => {
  test('embedded self-loop controls and label stay right of the node rectangle', () => {
    const layout = layoutEmbedded(selfLoopGraph, new Set(), 'self');
    const node = layout.nodes.repeat;
    expect(node).toBeDefined();
    if (node === undefined) return;
    const nodeRight = node.x + node.width;
    const html = renderEmbedded(selfLoopGraph, 'self');
    expectExteriorPath(html, selfLoopId, 'right', nodeRight, node.y + node.height, node.y);
    expect(labelX(html, selfLoopId)).toBeGreaterThan(nodeRight);
    expect(layout.width).toBeGreaterThan(nodeRight);
  });

  test('embedded later-to-earlier controls and label stay left of every node', () => {
    const layout = layoutEmbedded(backEdgeGraph, new Set(), 'back');
    const source = layout.nodes.later;
    const target = layout.nodes.earlier;
    expect(source).toBeDefined();
    expect(target).toBeDefined();
    if (source === undefined || target === undefined) return;
    const nodeLeft = Math.min(...Object.values(layout.nodes).map((node) => node.x));
    const html = renderEmbedded(backEdgeGraph, 'back');
    expectExteriorPath(html, backEdgeId, 'left', nodeLeft, source.y + source.height, target.y);
    expect(labelX(html, backEdgeId)).toBeLessThan(nodeLeft);
    expect(layout.nodeLeft).toBeGreaterThan(0);
  });

  test('root self-loop controls and label stay right of the node rectangle', () => {
    const html = renderToStaticMarkup(
      <svg>
        <title>Root self loop</title>
        <ParallelCanvasEdgeVisual
          back
          graphLeft={0}
          graphRight={200}
          id={selfLoopId}
          label="otherwise ×3"
          selfLoop
          siblingOffset={0}
          sourceX={100}
          sourceY={188}
          targetX={100}
          targetY={100}
        />
      </svg>
    );
    expectExteriorPath(html, selfLoopId, 'right', 200, 188, 100);
    expect(labelX(html, selfLoopId)).toBeGreaterThan(200);
  });

  test('root later-to-earlier controls and label stay left of the graph bounds', () => {
    const html = renderToStaticMarkup(
      <svg>
        <title>Root back edge</title>
        <ParallelCanvasEdgeVisual
          back
          graphLeft={0}
          graphRight={200}
          id={backEdgeId}
          label="when ×3"
          selfLoop={false}
          siblingOffset={0}
          sourceX={100}
          sourceY={300}
          targetX={100}
          targetY={100}
        />
      </svg>
    );
    expectExteriorPath(html, backEdgeId, 'left', 0, 300, 100);
    expect(labelX(html, backEdgeId)).toBeLessThan(0);
  });
});
