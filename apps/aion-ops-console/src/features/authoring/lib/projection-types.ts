export type SourceSpan = {
  start: number;
  end: number;
  line: number;
  column: number;
};

export type SemanticDeclaration = {
  name: string;
  kind:
    | 'workflow'
    | 'input'
    | 'signal'
    | 'outcome'
    | 'type'
    | 'field'
    | 'variant'
    | 'worker'
    | 'action'
    | 'child'
    | 'parameter'
    | 'step'
    | 'binding';
  documentation: string | null;
  span: SourceSpan;
};

export type SemanticEntry = {
  span: SourceSpan;
  type: string | null;
  declaration: SemanticDeclaration | null;
};

export type StepMarkers = { looped: boolean; forked: boolean; waits: boolean };
export type ProjectionStep = {
  name: string;
  documentation: string;
  span: SourceSpan;
  markers: StepMarkers;
};
export type ProjectionEdge = {
  id: string;
  source: string;
  target: string;
  kind: 'route' | 'fall_through' | 'after';
  label: string | null;
};
export type ProjectionChildCall = {
  id: string;
  parentStep: string;
  name: string;
  signature: string;
  span: SourceSpan;
};
export type GraphProjection = {
  steps: ProjectionStep[];
  edges: ProjectionEdge[];
  childCalls: ProjectionChildCall[];
};
export type SemanticIndex = { entries: SemanticEntry[]; graph: GraphProjection };

export type LayoutPosition = { x: number; y: number };
export type LayoutRecord = { positions: Record<string, LayoutPosition> };
