import {
  expectArray,
  expectBoolean,
  expectNumber,
  expectRecord,
  expectString,
  nullableString,
} from './facade-values';
import type {
  GraphProjection,
  ProjectionStep,
  ProjectionSubstepGraph,
  SemanticDeclaration,
  SemanticEntry,
  SemanticIndex,
  SourceSpan,
  StudioProjection,
} from './projection-types';

/** Parse the semantic index and recursively nested canvas projection. */
export function parseSemanticIndex(value: unknown): SemanticIndex {
  const record = expectRecord(value);
  return {
    entries: expectArray(record.entries, 'entries').map(parseSemanticEntry),
    graph: parseGraph(record.graph),
    studio: parseStudio(record.studio),
  };
}

function parseStudio(value: unknown): StudioProjection {
  const record = expectRecord(value);
  return {
    builtins: expectArray(record.builtins, 'studio.builtins').map((item) =>
      expectString(item, 'builtin')
    ),
    types: expectArray(record.types, 'studio.types').map((item) => {
      const type = expectRecord(item);
      const kind = expectString(type.kind, 'type.kind');
      if (kind !== 'record' && kind !== 'enum' && kind !== 'schema') {
        throw new Error('Invalid authoring response: type.kind');
      }
      return {
        name: expectString(type.name, 'type.name'),
        kind,
        fields: expectArray(type.fields, 'type.fields').map(parseStudioField),
        variants: expectArray(type.variants, 'type.variants').map((variant) =>
          expectString(variant, 'variant')
        ),
      };
    }),
    workers: expectArray(record.workers, 'studio.workers').map((item) => {
      const worker = expectRecord(item);
      return {
        name: expectString(worker.name, 'worker.name'),
        actions: expectArray(worker.actions, 'worker.actions').map((item) => {
          const action = expectRecord(item);
          return {
            name: expectString(action.name, 'action.name'),
            params: expectArray(action.params, 'action.params').map(parseStudioField),
            returnType: expectString(action.return_type, 'action.return_type'),
          };
        }),
      };
    }),
  };
}

function parseStudioField(value: unknown) {
  const field = expectRecord(value);
  return {
    name: expectString(field.name, 'field.name'),
    type: expectString(field.type, 'field.type'),
  };
}

function parseSemanticEntry(value: unknown): SemanticEntry {
  const record = expectRecord(value);
  return {
    span: parseSpan(record.span),
    type: nullableString(record.type, 'type'),
    declaration: record.declaration === null ? null : parseDeclaration(record.declaration),
  };
}

function parseDeclaration(value: unknown): SemanticDeclaration {
  const record = expectRecord(value);
  const kind = expectString(record.kind, 'kind');
  const kinds: SemanticDeclaration['kind'][] = [
    'workflow',
    'input',
    'signal',
    'outcome',
    'type',
    'field',
    'variant',
    'worker',
    'action',
    'const',
    'subflow',
    'child',
    'parameter',
    'step',
    'binding',
  ];
  if (!kinds.includes(kind as SemanticDeclaration['kind'])) {
    throw new Error('Invalid authoring response: declaration kind');
  }
  return {
    name: expectString(record.name, 'name'),
    kind: kind as SemanticDeclaration['kind'],
    documentation: nullableString(record.documentation, 'documentation'),
    span: parseSpan(record.span),
  };
}

function parseSpan(value: unknown): SourceSpan {
  const record = expectRecord(value);
  return {
    start: expectNumber(record.start, 'start'),
    end: expectNumber(record.end, 'end'),
    line: expectNumber(record.line, 'line'),
    column: expectNumber(record.column, 'column'),
  };
}

const projectionStepKinds: ProjectionStep['kind'][] = [
  'plain',
  'distribute',
  'sequence',
  'collect',
  'subflow_call',
  'decision',
];

function parseGraph(value: unknown): GraphProjection {
  const record = expectRecord(value);
  return {
    steps: expectArray(record.steps, 'graph.steps').map(parseProjectionStep),
    edges: expectArray(record.edges, 'graph.edges').map((item) => {
      const edge = expectRecord(item);
      const kind = edge.kind;
      if (kind !== 'route' && kind !== 'fall_through' && kind !== 'after') {
        throw new Error('Invalid authoring response: edge.kind');
      }
      return {
        id: expectString(edge.id, 'edge.id'),
        source: expectString(edge.source, 'edge.source'),
        target: expectString(edge.target, 'edge.target'),
        kind,
        label: nullableString(edge.label, 'edge.label'),
        back: expectBoolean(edge.back, 'edge.back'),
        visits: nullableString(edge.visits, 'edge.visits'),
      };
    }),
    childCalls: expectArray(record.child_calls, 'graph.child_calls').map((item) => {
      const child = expectRecord(item);
      return {
        id: expectString(child.id, 'child.id'),
        parentStep: expectString(child.parent_step, 'child.parent_step'),
        name: expectString(child.name, 'child.name'),
        signature: expectString(child.signature, 'child.signature'),
        span: parseSpan(child.span),
      };
    }),
  };
}

function parseProjectionStep(value: unknown): ProjectionStep {
  const step = expectRecord(value);
  const kind = expectString(step.kind, 'step.kind');
  if (!projectionStepKinds.includes(kind as ProjectionStep['kind'])) {
    throw new Error('Invalid authoring response: step.kind');
  }
  return {
    name: expectString(step.name, 'step.name'),
    documentation: expectString(step.documentation, 'step.documentation'),
    activities: expectArray(step.activities, 'step.activities').map((activity) =>
      expectString(activity, 'step.activity')
    ),
    span: parseSpan(step.span),
    kind: kind as ProjectionStep['kind'],
    region: nullableString(step.region, 'step.region'),
    distribution: step.distribution === null ? null : parseDistribution(step.distribution),
    collect: step.collect === null ? null : parseCollect(step.collect),
    subflow: step.subflow === null ? null : parseSubflow(step.subflow),
    substeps: expectArray(step.substeps, 'step.substeps').map(parseSubstepGraph),
    visits: nullableString(step.visits, 'step.visits'),
    decision: expectBoolean(step.decision, 'step.decision'),
    waits: expectBoolean(step.waits, 'step.waits'),
  };
}

function parseSubstepGraph(value: unknown): ProjectionSubstepGraph {
  const record = expectRecord(value);
  const scope = expectString(record.scope, 'substep.scope');
  if (scope !== 'body' && scope !== 'failure' && scope !== 'fork' && scope !== 'loop') {
    throw new Error('Invalid authoring response: substep.scope');
  }
  return {
    scope,
    index: expectNumber(record.index, 'substep.index'),
    graph: parseGraph(record.graph),
  };
}

function parseDistribution(value: unknown): NonNullable<ProjectionStep['distribution']> {
  const record = expectRecord(value);
  return {
    binding: expectString(record.binding, 'distribution.binding'),
    collection: expectString(record.collection, 'distribution.collection'),
  };
}

function parseCollect(value: unknown): NonNullable<ProjectionStep['collect']> {
  const record = expectRecord(value);
  return {
    binding: expectString(record.binding, 'collect.binding'),
    tolerant: expectBoolean(record.tolerant, 'collect.tolerant'),
    result: expectString(record.result, 'collect.result'),
  };
}

function parseSubflow(value: unknown): NonNullable<ProjectionStep['subflow']> {
  const record = expectRecord(value);
  return {
    name: expectString(record.name, 'subflow.name'),
    graph: record.graph === null ? null : parseGraph(record.graph),
  };
}
