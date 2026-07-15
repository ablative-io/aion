import dagre from 'dagre';

import type { GraphProjection, ProjectionStep, ProjectionSubstepGraph } from './projection-types';

/** Base box of one embedded graph node. */
export const EMBED_NODE_WIDTH = 200;
export const EMBED_NODE_HEIGHT = 88;
/** Square box of a pure-decision diamond node. */
export const DECISION_NODE_SIZE = 160;
/** Padding around an embedded graph inside its node. */
export const EMBED_PADDING = 16;
/** Height added by each collapsed scoped-substep section. */
export const EMBEDDED_SECTION_HEADER_HEIGHT = 36;
/** Lateral distance between parallel edge siblings. */
export const PARALLEL_EDGE_GAP = 24;
/** Clear space between a node boundary and a back/self-loop edge gutter. */
export const EDGE_GUTTER_CLEARANCE = 32;

export type NodeSize = { width: number; height: number };
export type PlacedNode = { x: number; y: number; width: number; height: number };
export type EmbeddedLayout = {
  width: number;
  height: number;
  nodeLeft: number;
  nodeRight: number;
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
  let width = base.width;
  let height = base.height + step.substeps.length * EMBEDDED_SECTION_HEADER_HEIGHT;
  const subflow = step.subflow?.graph;
  if (subflow !== null && subflow !== undefined && expanded.has(path)) {
    const inner = layoutEmbedded(subflow, expanded, path);
    width = Math.max(width, inner.width + EMBED_PADDING * 2);
    height += inner.height + EMBED_PADDING;
  }
  for (const scoped of step.substeps) {
    const scopedPath = substepGraphPath(path, scoped);
    if (!expanded.has(scopedPath)) continue;
    const inner = layoutEmbedded(scoped.graph, expanded, scopedPath);
    width = Math.max(width, inner.width + EMBED_PADDING * 2);
    height += inner.height + EMBED_PADDING;
  }
  return { width, height };
}

/** Expansion key for one statement-list-scoped substep graph. */
export function substepGraphPath(path: string, scoped: ProjectionSubstepGraph): string {
  return `${path}/@${scoped.scope}-${scoped.index}`;
}

/** Human-readable label for one statement-list-scoped substep graph. */
export function substepScopeLabel(scoped: ProjectionSubstepGraph): string {
  if (scoped.scope === 'body') return 'Body substeps';
  if (scoped.scope === 'failure') return 'Failure substeps';
  const occurrence = scoped.index + 1;
  return `${scoped.scope === 'fork' ? 'Fork' : 'Loop'} ${occurrence} substeps`;
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
  let minX = Number.POSITIVE_INFINITY;
  let maxX = Number.NEGATIVE_INFINITY;
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
    minX = Math.min(minX, node.x);
    maxX = Math.max(maxX, node.x + node.width);
    maxY = Math.max(maxY, node.y + node.height);
  }
  const rawWidth = Number.isFinite(minX) ? maxX - minX : 0;
  const siblingOffsets = parallelEdgeOffsets(graph.edges);
  let leftGutter = 0;
  let rightGutter = 0;
  let needsVerticalGutter = false;
  for (const edge of graph.edges) {
    const selfLoop = edge.source === edge.target;
    if (!(edge.back || selfLoop)) continue;
    needsVerticalGutter = true;
    const required = EDGE_GUTTER_CLEARANCE + edgeLaneSpread(siblingOffsets.get(edge.id) ?? 0);
    if (selfLoop) {
      rightGutter = Math.max(rightGutter, required);
    } else {
      leftGutter = Math.max(leftGutter, required);
    }
  }
  const verticalGutter = needsVerticalGutter ? EDGE_GUTTER_CLEARANCE : 0;
  const shiftX = leftGutter - (Number.isFinite(minX) ? minX : 0);
  for (const node of Object.values(nodes)) {
    node.x += shiftX;
    node.y += verticalGutter;
  }
  const nodeRight = leftGutter + rawWidth;
  return {
    width: Math.ceil(nodeRight + rightGutter),
    height: Math.ceil(maxY + verticalGutter * 2),
    nodeLeft: leftGutter,
    nodeRight,
    nodes,
  };
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

/** Extra gutter distance that keeps centered sibling offsets on unique lanes. */
export function edgeLaneSpread(siblingOffset: number): number {
  return Math.abs(siblingOffset) * 2 + (siblingOffset < 0 ? PARALLEL_EDGE_GAP / 2 : 0);
}

/**
 * Route a bottom handle to a top handle without crossing either endpoint node:
 * leave below the source, traverse a lateral lane, then enter from above.
 */
export function lateralEdgePath(
  from: { x: number; y: number },
  to: { x: number; y: number },
  laneX: number
): string {
  const sourceOuterY = from.y + EDGE_GUTTER_CLEARANCE;
  const targetOuterY = to.y - EDGE_GUTTER_CLEARANCE;
  return `M ${from.x} ${from.y} C ${from.x} ${sourceOuterY}, ${laneX} ${sourceOuterY}, ${laneX} ${sourceOuterY} L ${laneX} ${targetOuterY} C ${laneX} ${targetOuterY}, ${to.x} ${targetOuterY}, ${to.x} ${to.y}`;
}
