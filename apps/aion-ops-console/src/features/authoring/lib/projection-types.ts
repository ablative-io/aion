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
    | 'const'
    | 'subflow'
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

export type ProjectionStepKind =
  | 'plain'
  | 'distribute'
  | 'sequence'
  | 'collect'
  | 'subflow_call'
  | 'decision';
export type ProjectionDistribution = { binding: string; collection: string };
export type ProjectionCollect = { binding: string; tolerant: boolean; result: string };
export type ProjectionSubflow = { name: string; graph: GraphProjection | null };
export type ProjectionStep = {
  name: string;
  documentation: string;
  span: SourceSpan;
  kind: ProjectionStepKind;
  region: string | null;
  distribution: ProjectionDistribution | null;
  collect: ProjectionCollect | null;
  subflow: ProjectionSubflow | null;
  substeps: GraphProjection | null;
  visits: string | null;
  decision: boolean;
  waits: boolean;
  activities: string[];
};
export type ProjectionEdge = {
  id: string;
  source: string;
  target: string;
  kind: 'route' | 'fall_through' | 'after';
  label: string | null;
  back: boolean;
  visits: string | null;
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
export type StudioField = { name: string; type: string };
export type StudioType = {
  name: string;
  kind: 'record' | 'enum' | 'schema';
  fields: StudioField[];
  variants: string[];
};
export type StudioAction = {
  name: string;
  params: StudioField[];
  returnType: string;
};
export type StudioWorker = { name: string; actions: StudioAction[] };
export type StudioProjection = {
  builtins: string[];
  types: StudioType[];
  workers: StudioWorker[];
};
export type SemanticIndex = {
  entries: SemanticEntry[];
  graph: GraphProjection;
  studio: StudioProjection;
};

export type LayoutPosition = { x: number; y: number };
export type LayoutRecord = { positions: Record<string, LayoutPosition> };
