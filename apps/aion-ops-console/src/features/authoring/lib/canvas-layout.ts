import dagre from 'dagre';

import type { GraphProjection, ProjectionStep } from './projection-types';

/** Base box of one embedded graph node. */
export const EMBED_NODE_WIDTH = 200;
export const EMBED_NODE_HEIGHT = 88;
/** Square box of a pure-decision diamond node. */
export const DECISION_NODE_SIZE = 160;
/** Padding around an embedded graph inside its node. */
export const EMBED_PADDING = 16;
/** Height of a parent node's own header above its expanded graph. */
export const EMBEDDED_HEADER_HEIGHT = 128;
/** Lateral distance between parallel edge siblings. */
export const PARALLEL_EDGE_GAP = 24;

export type NodeSize = { width: number; height: number };
export type PlacedNode = { x: number; y: number; width: number; height: number };
export type EmbeddedLayout = {
  width: number;
  height: number;
  nodes: Record<string, PlacedNode>;
};

/**
 * The drawn size of one step node, growing when the node is an expanded
 * subflow or substep owner (its graph renders in place) and squaring when it
 * is a pure decision diamond. `path` is the node's expansion path — the
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
  const graph = step.subflow?.graph ?? step.substeps;
  if (graph === null || !expanded.has(path)) return base;
  const inner = layoutEmbedded(graph, expanded, path);
  return {
    width: Math.max(base.width, inner.width + EMBED_PADDING * 2),
    height: EMBEDDED_HEADER_HEIGHT + inner.height + EMBED_PADDING,
  };
}

/**
 * Lays out one embedded graph with dagre (top-to-bottom), returning top-left
 * positions in local coordinates plus the drawn extent. Nested expanded
 * graphs grow their owner nodes recursively.
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

/**
 * Stable lateral offsets for edges sharing an endpoint pair. Sorting by wire
 * id makes the assignment independent of response ordering.
 */
export function parallelEdgeOffsets(
  edges: readonly { id: string; source: string; target: string }[]
): ReadonlyMap<string, number> {
  const groups = new Map<string, { id: string }[]>();
  for (const edge of edges) {
    const key = `${edge.source}\u0000${edge.target}`;
    const group = groups.get(key) ?? [];
    group.push(edge);
    groups.set(key, group);
  }
  const offsets = new Map<string, number>();
  for (const group of groups.values()) {
    group.sort((left, right) => left.id.localeCompare(right.id));
    for (const [index, edge] of group.entries()) {
      offsets.set(edge.id, (index - (group.length - 1) / 2) * PARALLEL_EDGE_GAP);
    }
  }
  return offsets;
}
