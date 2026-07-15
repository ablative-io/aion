import { Timer } from 'lucide-react';

import { cn } from '@/lib/utils';
import {
  EDGE_GUTTER_CLEARANCE,
  EMBED_PADDING,
  edgeLabel,
  edgeLaneSpread,
  lateralEdgePath,
  layoutEmbedded,
  parallelEdgeOffsets,
  type PlacedNode,
  substepGraphPath,
  substepScopeLabel,
} from '../lib/canvas-layout';
import type {
  GraphProjection,
  ProjectionEdge,
  ProjectionStep,
  ProjectionSubstepGraph,
} from '../lib/projection-types';

/**
 * The canvas node vocabulary (AWL-FLOW-VOCABULARY rev 3 §6), free of any
 * reactflow dependency so the same bodies render recursively inside an
 * expanded subflow call: plain cards, ⫴ distribute / sequence openers,
 * ⫵ collect closers, ×N region members, expandable subflow calls, and
 * decision diamonds (pure steps as the diamond, mixed steps with a
 * trailing diamond for their outcome arms).
 */
export type StepBodyProps = {
  step: ProjectionStep;
  /** `/`-joined chain of step names from the canvas root (expansion key). */
  path: string;
  expanded: ReadonlySet<string>;
  diagnostic?: 'primary' | 'cascade' | undefined;
  selected?: boolean;
  onJumpTo: (byteOffset: number) => void;
  onToggleSubflow: (path: string) => void;
};

export function StepNodeBody(props: StepBodyProps) {
  if (props.step.kind === 'decision') return <DecisionDiamond {...props} />;
  const { step, diagnostic, selected } = props;
  return (
    <article
      aria-label={`Step ${step.name}`}
      className={cn(
        'min-w-52 rounded-xl border bg-surface-elevated shadow-sm transition-[border-color,box-shadow]',
        selected === true && 'border-accent-primary shadow-md',
        selected !== true && diagnostic === undefined && 'border-border',
        step.region !== null && 'border-l-4 border-l-accent-primary/60',
        diagnostic === 'primary' && 'border-destructive bg-destructive/10',
        diagnostic === 'cascade' && 'border-muted-foreground/40 opacity-70'
      )}
      data-kind={step.kind}
      data-region={step.region ?? undefined}
    >
      <button
        className="nodrag w-full rounded-xl p-3 text-left outline-none focus-visible:ring-2 focus-visible:ring-accent-primary focus-visible:ring-offset-2 focus-visible:ring-offset-surface-base"
        onClick={() => props.onJumpTo(step.span.start)}
        type="button"
      >
        <span className="flex items-center justify-between gap-2">
          <span className="truncate font-mono font-semibold text-foreground text-xs">
            {step.name}
          </span>
          <StepBadges step={step} />
        </span>
        <KindLabel step={step} />
        <span className="mt-2 block text-foreground text-sm leading-snug">
          {step.documentation || 'Undocumented step'}
        </span>
        {diagnostic !== undefined && (
          <span
            className={cn(
              'mt-2 block text-xs',
              diagnostic === 'primary' ? 'text-destructive' : 'text-muted-foreground'
            )}
          >
            {diagnostic === 'primary' ? 'Primary error' : 'Downstream cascade'}
          </span>
        )}
      </button>
      {step.subflow !== null && <SubflowSection {...props} />}
      {step.substeps.map((scoped) => (
        <SubstepSection {...props} key={`${scoped.scope}:${scoped.index}`} scoped={scoped} />
      ))}
    </article>
  );
}

/** A body-less step with only outcome arms: the decision diamond itself. */
function DecisionDiamond({ step, diagnostic, selected, onJumpTo }: StepBodyProps) {
  return (
    <article
      aria-label={`Decision ${step.name}`}
      className="relative flex size-40 items-center justify-center"
      data-kind="decision"
      data-region={step.region ?? undefined}
    >
      <span
        aria-hidden="true"
        className={cn(
          'absolute inset-5 rotate-45 rounded-lg border bg-surface-elevated shadow-sm',
          selected === true && 'border-accent-primary shadow-md',
          selected !== true && diagnostic === undefined && 'border-border',
          diagnostic === 'primary' && 'border-destructive bg-destructive/10',
          diagnostic === 'cascade' && 'border-muted-foreground/40 opacity-70'
        )}
      />
      <button
        className="nodrag relative z-10 max-w-28 p-2 text-center outline-none focus-visible:ring-2 focus-visible:ring-accent-primary"
        onClick={() => onJumpTo(step.span.start)}
        type="button"
      >
        <span className="block truncate font-mono font-semibold text-foreground text-xs">
          {step.name}
        </span>
        <span className="mt-1 block text-[10px] text-muted-foreground uppercase tracking-wide">
          decision
        </span>
        <StepBadges step={step} />
      </button>
    </article>
  );
}

function StepBadges({ step }: { step: ProjectionStep }) {
  return (
    <span className="flex shrink-0 items-center gap-1.5 text-muted-foreground">
      {step.region !== null && (
        <span
          aria-label={`Runs once per item of ${step.region}`}
          className="rounded-full border border-accent-primary/50 px-1.5 font-semibold text-[10px] text-accent-primary"
          role="img"
          title={`Runs once per item of ${step.region}`}
        >
          ×N
        </span>
      )}
      {step.waits && <Timer aria-label="Waits" className="size-3.5" />}
      {step.decision && step.kind !== 'decision' && (
        <span
          aria-label="Has outcome arms"
          className="text-accent-primary"
          role="img"
          title="Outcome arms"
        >
          ◇
        </span>
      )}
    </span>
  );
}

/** The kind-specific label row: ⫴ opener, ⫵ closer, subflow call. */
function KindLabel({ step }: { step: ProjectionStep }) {
  if ((step.kind === 'distribute' || step.kind === 'sequence') && step.distribution !== null) {
    return (
      <span
        className={cn(
          'mt-2 flex items-center gap-1.5 font-mono text-xs',
          step.kind === 'sequence' ? 'text-warning' : 'text-accent-primary'
        )}
      >
        <span aria-hidden="true" className="font-semibold text-sm">
          ⫴
        </span>
        <span className="truncate">
          {step.distribution.binding} in {step.distribution.collection}
        </span>
        <span className="ml-auto shrink-0 text-[10px] text-muted-foreground uppercase tracking-wide">
          {step.kind === 'sequence' ? 'sequence · in order' : 'distribute'}
        </span>
      </span>
    );
  }
  if (step.kind === 'collect' && step.collect !== null) {
    return (
      <span className="mt-2 flex items-center gap-1.5 font-mono text-accent-primary text-xs">
        <span aria-hidden="true" className="font-semibold text-sm">
          ⫵
        </span>
        <span className="truncate">
          collect {step.collect.binding}
          {step.collect.tolerant ? '?' : ''} → {step.collect.result}
        </span>
      </span>
    );
  }
  return null;
}

/** Collapsed-by-default subflow call, expandable in place to its graph. */
function SubflowSection({ step, path, expanded, onJumpTo, onToggleSubflow }: StepBodyProps) {
  const subflow = step.subflow;
  if (subflow === null) return null;
  const isExpanded = subflow.graph !== null && expanded.has(path);
  return (
    <div className="border-border border-t px-3 pb-3">
      <span className="mt-2 flex items-center justify-between gap-2">
        <span className="truncate font-mono text-accent-primary text-xs">
          subflow {subflow.name}
        </span>
        {subflow.graph !== null && (
          <button
            aria-expanded={isExpanded}
            aria-label={`${isExpanded ? 'Collapse' : 'Expand'} subflow ${subflow.name}`}
            className="nodrag shrink-0 rounded-md px-2 py-1 text-muted-foreground text-xs outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-accent-primary"
            onClick={() => onToggleSubflow(path)}
            type="button"
          >
            {isExpanded ? 'Collapse' : 'Expand'}
          </button>
        )}
      </span>
      {isExpanded && subflow.graph !== null && (
        <div className="mt-2 overflow-auto rounded-lg border border-border border-dashed bg-surface-base p-2">
          <SubflowGraph
            expanded={expanded}
            graph={subflow.graph}
            onJumpTo={onJumpTo}
            onToggleSubflow={onToggleSubflow}
            path={path}
          />
        </div>
      )}
    </div>
  );
}

/** One collapsed-by-default statement-list-scoped sibling graph. */
function SubstepSection({
  step,
  path,
  expanded,
  onJumpTo,
  onToggleSubflow,
  scoped,
}: StepBodyProps & { scoped: ProjectionSubstepGraph }) {
  const scopedPath = substepGraphPath(path, scoped);
  const scopeLabel = substepScopeLabel(scoped);
  const isExpanded = expanded.has(scopedPath);
  return (
    <div className="border-border border-t px-3 pb-3">
      <span className="mt-2 flex items-center justify-between gap-2">
        <span className="truncate font-mono text-accent-primary text-xs">{scopeLabel}</span>
        <button
          aria-expanded={isExpanded}
          aria-label={`${isExpanded ? 'Collapse' : 'Expand'} ${scopeLabel} of ${step.name}`}
          className="nodrag shrink-0 rounded-md px-2 py-1 text-muted-foreground text-xs outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-accent-primary"
          onClick={() => onToggleSubflow(scopedPath)}
          type="button"
        >
          {isExpanded ? 'Collapse' : 'Expand'}
        </button>
      </span>
      {isExpanded && (
        <div className="mt-2 overflow-auto rounded-lg border border-border border-dashed bg-surface-base p-2">
          <SubflowGraph
            expanded={expanded}
            graph={scoped.graph}
            label={`${scopeLabel} graph of ${step.name}`}
            onJumpTo={onJumpTo}
            onToggleSubflow={onToggleSubflow}
            path={scopedPath}
          />
        </div>
      )}
    </div>
  );
}

export type SubflowGraphProps = {
  graph: GraphProjection;
  /** Expansion path of the owner node this graph renders inside. */
  path: string;
  /** Accessible graph name; defaults to the historical subflow label. */
  label?: string;
  expanded: ReadonlySet<string>;
  onJumpTo: (byteOffset: number) => void;
  onToggleSubflow: (path: string) => void;
};

/**
 * The recursive canvas: one subflow's own graph drawn with the same node
 * vocabulary, laid out with dagre and connected with SVG edges (back
 * edges labeled ×N).
 */
export function SubflowGraph({
  graph,
  path,
  label: graphLabel,
  expanded,
  onJumpTo,
  onToggleSubflow,
}: SubflowGraphProps) {
  const layout = layoutEmbedded(graph, expanded, path);
  const siblingOffsets = parallelEdgeOffsets(graph.edges);
  const width = layout.width + EMBED_PADDING;
  const height = layout.height + EMBED_PADDING;
  return (
    <section
      aria-label={graphLabel ?? `Subflow graph ${path}`}
      className="relative"
      style={{ width, height }}
    >
      <svg aria-hidden="true" className="absolute inset-0" height={height} width={width}>
        <title>Embedded graph edges</title>
        {graph.edges.map((edge) => {
          const source = layout.nodes[edge.source];
          const target = layout.nodes[edge.target];
          if (source === undefined || target === undefined) return null;
          return (
            <EmbeddedEdge
              edge={edge}
              key={edge.id}
              nodeLeft={layout.nodeLeft}
              nodeRight={layout.nodeRight}
              siblingOffset={siblingOffsets.get(edge.id) ?? 0}
              source={source}
              target={target}
            />
          );
        })}
      </svg>
      {graph.steps.map((step) => {
        const placed = layout.nodes[step.name];
        if (placed === undefined) return null;
        return (
          <div
            className="absolute"
            key={step.name}
            style={{ left: placed.x, top: placed.y, width: placed.width }}
          >
            <StepNodeBody
              expanded={expanded}
              onJumpTo={onJumpTo}
              onToggleSubflow={onToggleSubflow}
              path={`${path}/${step.name}`}
              step={step}
            />
          </div>
        );
      })}
    </section>
  );
}

type EmbeddedEdgeProps = {
  edge: ProjectionEdge;
  nodeLeft: number;
  nodeRight: number;
  siblingOffset: number;
  source: PlacedNode;
  target: PlacedNode;
};

function EmbeddedEdge({
  edge,
  nodeLeft,
  nodeRight,
  siblingOffset,
  source,
  target,
}: EmbeddedEdgeProps) {
  const from = { x: source.x + source.width / 2, y: source.y + source.height };
  const to = { x: target.x + target.width / 2, y: target.y };
  const label = edgeLabel(edge);
  const selfLoop = edge.source === edge.target;
  const gutterDistance = EDGE_GUTTER_CLEARANCE + edgeLaneSpread(siblingOffset);
  const direction = to.y >= from.y ? 1 : -1;
  const controlDistance = Math.max(24, Math.abs(to.y - from.y) / 2);
  let d: string;
  let mid: { x: number; y: number };
  if (selfLoop) {
    const laneX = nodeRight + gutterDistance;
    d = lateralEdgePath(from, to, laneX);
    mid = { x: laneX, y: (from.y + to.y) / 2 };
  } else if (edge.back) {
    const laneX = nodeLeft - gutterDistance;
    d = lateralEdgePath(from, to, laneX);
    mid = { x: laneX, y: (from.y + to.y) / 2 };
  } else {
    d = `M ${from.x} ${from.y} C ${from.x + siblingOffset} ${from.y + direction * controlDistance}, ${to.x + siblingOffset} ${to.y - direction * controlDistance}, ${to.x} ${to.y}`;
    mid = {
      x: (from.x + to.x) / 2 + siblingOffset + 6,
      y: (from.y + to.y) / 2,
    };
  }
  return (
    <g>
      <path
        d={d}
        data-edge-id={edge.id}
        fill="none"
        stroke={edge.kind === 'route' ? 'var(--accent-primary)' : 'var(--muted-foreground)'}
        strokeDasharray={edge.kind === 'fall_through' ? '6 5' : undefined}
        strokeWidth={edge.kind === 'route' ? 1.5 : 1}
      />
      {label !== null && (
        <text
          data-label-for={edge.id}
          fill="var(--muted-foreground)"
          fontSize={10}
          fontWeight={600}
          x={mid.x}
          y={mid.y}
        >
          {label}
        </text>
      )}
    </g>
  );
}
