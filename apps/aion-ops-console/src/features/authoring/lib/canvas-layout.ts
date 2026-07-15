import dagre from 'dagre';

import type { GraphProjection, ProjectionStep } from './projection-types';

/** Base box of one embedded (subflow-graph) node. */
export const EMBED_NODE_WIDTH = 200;
export const EMBED_NODE_HEIGHT = 88;
/** Square box of a pure-decision diamond node. */
export const DECISION_NODE_SIZE = 160;
/** Padding around an embedded subflow graph inside its node. */
export const EMBED_PADDING = 16;
/** Height of a subflow-call node's own header above its expanded graph. */
export const SUBFLOW_HEADER_HEIGHT = 128;

export type NodeSize = { width: number; height: number };
export type PlacedNode = { x: number; y: number; width: number; height: number };
export type EmbeddedLayout = {
  width: number;
  height: number;
  nodes: Record<string, PlacedNode>;
};

/**
 * The drawn size of one step node, growing when the node is an expanded
 * subflow call (its own graph renders in place) and squaring when it is a
 * pure decision diamond. `path` is the node's expansion path — the
 * `/`-joined chain of step names from the canvas root.
 */
export function nodeSize(
  step: ProjectionStep,
  path: string,
  expanded: ReadonlySet<string>,
  base: NodeSize
): NodeSize {
  if (step.kind === 'decision') {
    return { width: DECISION_NODE_SIZE, height: DECISION_NODE_SIZE };
  }
  const graph = step.subflow?.graph ?? null;
  if (graph === null || !expanded.has(path)) return base;
  const inner = layoutEmbedded(graph, expanded, path);
  return {
    width: Math.max(base.width, inner.width + EMBED_PADDING * 2),
    height: SUBFLOW_HEADER_HEIGHT + inner.height + EMBED_PADDING,
  };
}

/**
 * Lays out one embedded subflow graph with dagre (top-to-bottom), returning
 * top-left positions in local coordinates plus the drawn extent. Nested
 * expanded subflow calls grow their nodes recursively.
 */
export function layoutEmbedded(
  graph: GraphProjection,
  expanded: ReadonlySet<string>,
  pathPrefix: string
): EmbeddedLayout {
  const dagreGraph = new dagre.graphlib.Graph()
    .setDefaultEdgeLabel(() => ({}))
    .setGraph({ rankdir: 'TB', nodesep: 32, ranksep: 48 });
  const sizes = new Map<string, NodeSize>();
  for (const step of graph.steps) {
    const size = nodeSize(step, `${pathPrefix}/${step.name}`, expanded, {
      width: EMBED_NODE_WIDTH,
      height: EMBED_NODE_HEIGHT,
    });
    sizes.set(step.name, size);
    dagreGraph.setNode(step.name, size);
  }
  for (const edge of graph.edges) dagreGraph.setEdge(edge.source, edge.target);
  dagre.layout(dagreGraph);

  const nodes: Record<string, PlacedNode> = {};
  let maxX = 0;
  let maxY = 0;
  for (const step of graph.steps) {
    const placed = dagreGraph.node(step.name);
    const size = sizes.get(step.name) ?? { width: EMBED_NODE_WIDTH, height: EMBED_NODE_HEIGHT };
    const node = {
      x: placed.x - size.width / 2,
      y: placed.y - size.height / 2,
      width: size.width,
      height: size.height,
    };
    nodes[step.name] = node;
    maxX = Math.max(maxX, node.x + node.width);
    maxY = Math.max(maxY, node.y + node.height);
  }
  return { width: Math.ceil(maxX), height: Math.ceil(maxY), nodes };
}

/** The label a control edge is drawn with — back edges carry `×N`. */
export function edgeLabel(edge: {
  label: string | null;
  back: boolean;
  visits: string | null;
}): string | null {
  if (edge.back) {
    const bound = edge.visits === null ? null : `×${edge.visits}`;
    if (edge.label === null) return bound;
    return bound === null ? edge.label : `${edge.label} ${bound}`;
  }
  return edge.label;
}
