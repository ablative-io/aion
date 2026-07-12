import dagre from 'dagre';
import { useCallback, useEffect, useMemo, useState } from 'react';
import ReactFlow, {
  applyNodeChanges,
  Background,
  type Connection,
  Controls,
  type Edge,
  MarkerType,
  type Node,
  type NodeChange,
} from 'reactflow';
import 'reactflow/dist/style.css';

import type { AwlDiagnostic, AwlDocument, EditResult, GestureOperation } from '../lib/facade';
import { authoringFacade } from '../lib/facade';
import { diagnosticsByStep } from '../lib/projection';
import type { GraphProjection, LayoutRecord, ProjectionStep } from '../lib/projection-types';
import { CanvasGestureControls } from './CanvasGestureControls';
import { type CanvasNodeData, ChildCanvasNode, StepCanvasNode } from './CanvasNode';

const NODE_WIDTH = 240;
const STEP_HEIGHT = 140;
const CHILD_HEIGHT = 120;
const nodeTypes = { step: StepCanvasNode, child: ChildCanvasNode };

export type AuthoringCanvasProps = {
  path: string;
  graph: GraphProjection;
  diagnostics: readonly AwlDiagnostic[];
  documents: readonly AwlDocument[];
  routeTargets: readonly string[];
  selectedStep: string | null;
  onJumpToSpan: (byteOffset: number) => void;
  onOpenDocument: (path: string) => void;
  onGesture: (operation: GestureOperation) => Promise<EditResult>;
  mode?: 'edit' | 'run';
};

export function AuthoringCanvas({
  path,
  graph,
  diagnostics,
  documents,
  routeTargets,
  selectedStep,
  onJumpToSpan,
  onOpenDocument,
  onGesture,
  mode = 'edit',
}: AuthoringCanvasProps) {
  const [layout, setLayout] = useState<LayoutRecord>({ positions: {} });
  const [layoutError, setLayoutError] = useState<string | null>(null);
  const [gestureError, setGestureError] = useState<string | null>(null);
  const [gestureWorking, setGestureWorking] = useState(false);
  const [proseEditingStep, setProseEditingStep] = useState<string | null>(null);
  const diagnosticTones = useMemo(
    () => diagnosticsByStep(graph, diagnostics),
    [diagnostics, graph]
  );
  const migrateStepLayout = useCallback(
    async (result: EditResult) => {
      if (result.rename?.kind !== 'step') return;
      const position = layout.positions[result.rename.from];
      if (position === undefined) return;
      const positions = { ...layout.positions };
      delete positions[result.rename.from];
      positions[result.rename.to] = position;
      const migrated = { positions };
      setLayout(migrated);
      await authoringFacade.saveLayout(path, migrated);
    },
    [layout, path]
  );

  const performGesture = useCallback(
    async (operation: GestureOperation) => {
      setGestureWorking(true);
      setGestureError(null);
      try {
        const result = await onGesture(operation);
        await migrateStepLayout(result);
        return result;
      } catch (error) {
        setGestureError(error instanceof Error ? error.message : 'Gesture was refused');
        throw error;
      } finally {
        setGestureWorking(false);
      }
    },
    [migrateStepLayout, onGesture]
  );

  const projected = useMemo(
    () =>
      projectFlow(
        graph,
        layout,
        documents,
        diagnosticTones,
        selectedStep,
        onJumpToSpan,
        onOpenDocument,
        proseEditingStep,
        setProseEditingStep,
        performGesture,
        mode
      ),
    [
      diagnosticTones,
      documents,
      graph,
      layout,
      onJumpToSpan,
      onOpenDocument,
      performGesture,
      proseEditingStep,
      selectedStep,
      mode,
    ]
  );
  const [nodes, setNodes] = useState(projected.nodes);

  useEffect(() => {
    let active = true;
    setLayout({ positions: {} });
    setLayoutError(null);
    void authoringFacade
      .loadLayout(path)
      .then((record) => {
        if (active) setLayout(record);
      })
      .catch((error: unknown) => {
        if (active) setLayoutError(error instanceof Error ? error.message : 'Layout unavailable');
      });
    return () => {
      active = false;
    };
  }, [path]);

  useEffect(() => setNodes(projected.nodes), [projected.nodes]);

  function onNodesChange(changes: NodeChange[]) {
    setNodes((current) => applyNodeChanges(changes, current));
  }

  function persistNodePosition(node: Node<CanvasNodeData>) {
    if (mode === 'run') return;
    if (node.type !== 'step') return;
    const positions = { ...layout.positions, [node.id]: node.position };
    const next = { positions };
    setLayout(next);
    setLayoutError(null);
    void authoringFacade.saveLayout(path, next).catch((error: unknown) => {
      setLayoutError(error instanceof Error ? error.message : 'Layout could not be saved');
    });
  }

  const connect = useCallback(
    (connection: Connection) => {
      if (connection.source === null || connection.target === null) return;
      if (!graph.steps.some((step) => step.name === connection.target)) return;
      void performGesture({
        type: 'add_outcome_route',
        source: connection.source,
        target: connection.target,
        name: `to_${connection.target}`,
        guard: { type: 'otherwise' },
      });
    },
    [graph.steps, performGesture]
  );

  return (
    <section
      className="relative min-h-[32rem] flex-1 bg-surface-elevated"
      aria-label="Workflow canvas"
    >
      <ReactFlow
        aria-label="AWL workflow graph"
        edges={projected.edges}
        fitView
        fitViewOptions={{ padding: 0.2 }}
        nodeTypes={nodeTypes}
        nodes={nodes}
        nodesConnectable={mode === 'edit' && !gestureWorking}
        onConnect={connect}
        onNodeDragStop={(_event, node) => persistNodePosition(node)}
        onNodesChange={onNodesChange}
        panOnScroll
        proOptions={{ hideAttribution: true }}
      >
        <Background color="var(--border)" gap={24} size={1} />
        <Controls showInteractive={false} />
      </ReactFlow>
      {mode === 'edit' && (
        <CanvasGestureControls
          disabled={gestureWorking}
          graph={graph}
          onBeginProseEdit={setProseEditingStep}
          onGesture={performGesture}
          routeTargets={routeTargets}
          selectedStep={selectedStep}
        />
      )}
      <div className="pointer-events-none absolute right-3 bottom-3 rounded-lg border border-border bg-surface-base/95 px-3 py-2 text-muted-foreground text-xs shadow-sm">
        <span>
          {mode === 'run'
            ? 'Live run · nodes follow workflow events'
            : 'Tab through controls · connect handles to draw an otherwise route'}
        </span>
        {layoutError !== null && <span className="ml-2 text-destructive">{layoutError}</span>}
        {gestureError !== null && <span className="ml-2 text-destructive">{gestureError}</span>}
      </div>
    </section>
  );
}

function projectFlow(
  graph: GraphProjection,
  layout: LayoutRecord,
  documents: readonly AwlDocument[],
  diagnosticTones: ReadonlyMap<string, 'primary' | 'cascade'>,
  selectedStep: string | null,
  onJumpToSpan: (byteOffset: number) => void,
  onOpenDocument: (path: string) => void,
  proseEditingStep: string | null,
  onBeginProseEdit: (step: string | null) => void,
  onGesture: (operation: GestureOperation) => Promise<EditResult>,
  mode: 'edit' | 'run'
): { nodes: Node<CanvasNodeData>[]; edges: Edge[] } {
  const dagreGraph = new dagre.graphlib.Graph()
    .setDefaultEdgeLabel(() => ({}))
    .setGraph({ rankdir: 'TB', nodesep: 48, ranksep: 72 });
  for (const step of graph.steps) {
    dagreGraph.setNode(step.name, { width: NODE_WIDTH, height: STEP_HEIGHT });
  }
  for (const child of graph.childCalls) {
    dagreGraph.setNode(child.id, { width: NODE_WIDTH, height: CHILD_HEIGHT });
  }
  for (const edge of graph.edges) dagreGraph.setEdge(edge.source, edge.target);
  for (const child of graph.childCalls) dagreGraph.setEdge(child.parentStep, child.id);
  dagre.layout(dagreGraph);

  const nodes: Node<CanvasNodeData>[] = graph.steps.map((step) =>
    stepNode(
      step,
      dagreGraph.node(step.name),
      layout,
      diagnosticTones,
      selectedStep,
      onJumpToSpan,
      proseEditingStep,
      onBeginProseEdit,
      onGesture,
      mode
    )
  );
  for (const child of graph.childCalls) {
    const document = documents.find((item) => item.name === child.name);
    const position = topLeft(dagreGraph.node(child.id));
    nodes.push({
      id: child.id,
      type: 'child',
      position,
      draggable: false,
      focusable: false,
      selected: selectedStep === child.parentStep,
      data: {
        kind: 'child',
        name: child.name,
        label: child.signature,
        childDocumentPath: document?.path,
        onActivate: () => onJumpToSpan(child.span.start),
        onOpenChild: document === undefined ? undefined : () => onOpenDocument(document.path),
      },
    });
  }

  const edges: Edge[] = graph.edges.map((edge) => ({
    id: edge.id,
    source: edge.source,
    target: edge.target,
    label: edge.label ?? undefined,
    type: 'smoothstep',
    markerEnd: { type: MarkerType.ArrowClosed, color: edgeColor(edge.kind) },
    style: {
      stroke: edgeColor(edge.kind),
      strokeWidth: edge.kind === 'route' ? 2 : 1.5,
      strokeDasharray:
        edge.kind === 'fall_through' ? '6 5' : edge.kind === 'after' ? '2 5' : undefined,
    },
    labelStyle: { fill: 'var(--muted-foreground)', fontSize: 11, fontWeight: 600 },
    labelBgStyle: { fill: 'var(--surface-base)', fillOpacity: 0.9 },
  }));
  for (const child of graph.childCalls) {
    edges.push({
      id: `call:${child.id}`,
      source: child.parentStep,
      target: child.id,
      label: 'child',
      type: 'smoothstep',
      markerEnd: { type: MarkerType.ArrowClosed, color: 'var(--accent-primary)' },
      style: { stroke: 'var(--accent-primary)', strokeDasharray: '4 4' },
      labelStyle: { fill: 'var(--muted-foreground)', fontSize: 11 },
      labelBgStyle: { fill: 'var(--surface-base)', fillOpacity: 0.9 },
    });
  }
  return { nodes, edges };
}

function stepNode(
  step: ProjectionStep,
  auto: { x: number; y: number; width: number; height: number },
  layout: LayoutRecord,
  tones: ReadonlyMap<string, 'primary' | 'cascade'>,
  selectedStep: string | null,
  onJumpToSpan: (byteOffset: number) => void,
  proseEditingStep: string | null,
  onBeginProseEdit: (step: string | null) => void,
  onGesture: (operation: GestureOperation) => Promise<EditResult>,
  mode: 'edit' | 'run'
): Node<CanvasNodeData> {
  return {
    id: step.name,
    type: 'step',
    position: layout.positions[step.name] ?? topLeft(auto),
    focusable: false,
    draggable: mode === 'edit',
    selected: selectedStep === step.name,
    data: {
      kind: 'step',
      name: step.name,
      label: step.documentation,
      markers: step.markers,
      diagnostic: tones.get(step.name),
      onActivate: () => onJumpToSpan(step.span.start),
      proseEditing: proseEditingStep === step.name,
      onBeginProseEdit: () => onBeginProseEdit(step.name),
      onCancelProseEdit: () => onBeginProseEdit(null),
      onSaveProse: async (prose) => {
        await onGesture({ type: 'edit_prose', step: step.name, prose });
        onBeginProseEdit(null);
      },
    },
  };
}

function topLeft(node: { x: number; y: number; width: number; height: number }) {
  return { x: node.x - node.width / 2, y: node.y - node.height / 2 };
}

function edgeColor(kind: 'route' | 'fall_through' | 'after') {
  if (kind === 'route') return 'var(--accent-primary)';
  if (kind === 'after') return 'var(--warning)';
  return 'var(--muted-foreground)';
}
