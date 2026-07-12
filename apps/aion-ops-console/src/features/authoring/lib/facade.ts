import type {
  GraphProjection,
  LayoutPosition,
  LayoutRecord,
  SemanticDeclaration,
  SemanticEntry,
  SemanticIndex,
  SourceSpan,
} from './projection-types';

export type DiagnosticClass = 'error' | 'emit_subset';

export type AwlDiagnostic = {
  class: DiagnosticClass;
  message: string;
  line: number;
  column: number;
};

export type CheckResult = {
  ok: boolean;
  deploysGreen: boolean;
  steps: number | null;
  diagnostics: AwlDiagnostic[];
  semantic: SemanticIndex | null;
};

export type AwlDocument = { path: string; name: string };

export class AuthoringWorkspaceNotConfiguredError extends Error {
  constructor() {
    super('authoring workspace not configured');
    this.name = 'AuthoringWorkspaceNotConfiguredError';
  }
}

type Fetch = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

export function createAuthoringFacade(fetchImpl: Fetch = globalThis.fetch.bind(globalThis)) {
  async function request(
    path: string,
    init?: RequestInit,
    documentEndpoint = false
  ): Promise<unknown> {
    const response = await fetchImpl(path, init);
    if (documentEndpoint && response.status === 404) {
      throw new AuthoringWorkspaceNotConfiguredError();
    }
    if (!response.ok) {
      throw new Error(`Authoring request failed (${response.status})`);
    }
    return response.json();
  }

  return {
    async check(source: string, path?: string): Promise<CheckResult> {
      const value = expectRecord(
        await request(
          '/awl/check',
          jsonInit('POST', path === undefined ? { source } : { source, path })
        )
      );
      return {
        ok: expectBoolean(value.ok, 'ok'),
        deploysGreen: expectBoolean(value.deploys_green, 'deploys_green'),
        steps: value.steps === null ? null : expectNumber(value.steps, 'steps'),
        diagnostics: expectArray(value.diagnostics, 'diagnostics').map(parseDiagnostic),
        semantic: value.semantic === null ? null : parseSemanticIndex(value.semantic),
      };
    },
    async format(source: string): Promise<string> {
      const value = expectRecord(await request('/awl/fmt', jsonInit('POST', { source })));
      return expectString(value.formatted, 'formatted');
    },
    async listDocuments(): Promise<AwlDocument[]> {
      const value = await request('/awl/documents', undefined, true);
      return expectArray(value, 'documents').map((entry) => {
        const record = expectRecord(entry);
        return { path: expectString(record.path, 'path'), name: expectString(record.name, 'name') };
      });
    },
    async loadDocument(path: string): Promise<string> {
      const value = expectRecord(
        await request(`/awl/documents/${encodeURIComponent(path)}`, undefined, true)
      );
      return expectString(value.source, 'source');
    },
    async saveDocument(path: string, source: string): Promise<void> {
      await request(
        `/awl/documents/${encodeURIComponent(path)}`,
        jsonInit('PUT', { source }),
        true
      );
    },
    async loadLayout(path: string): Promise<LayoutRecord> {
      return parseLayout(await request(`/awl/layout/${encodeURIComponent(path)}`, undefined, true));
    },
    async saveLayout(path: string, layout: LayoutRecord): Promise<LayoutRecord> {
      return parseLayout(
        await request(`/awl/layout/${encodeURIComponent(path)}`, jsonInit('PUT', layout), true)
      );
    },
  };
}

export const authoringFacade = createAuthoringFacade();

function jsonInit(method: string, body: unknown): RequestInit {
  return { method, headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) };
}

function parseDiagnostic(value: unknown): AwlDiagnostic {
  const record = expectRecord(value);
  const diagnosticClass = record.class;
  if (diagnosticClass !== 'error' && diagnosticClass !== 'emit_subset') {
    throw new Error('Invalid authoring response: class');
  }
  return {
    class: diagnosticClass,
    message: expectString(record.message, 'message'),
    line: expectNumber(record.line, 'line'),
    column: expectNumber(record.column, 'column'),
  };
}

function parseSemanticIndex(value: unknown): SemanticIndex {
  const record = expectRecord(value);
  return {
    entries: expectArray(record.entries, 'entries').map(parseSemanticEntry),
    graph: parseGraph(record.graph),
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

function parseGraph(value: unknown): GraphProjection {
  const record = expectRecord(value);
  return {
    steps: expectArray(record.steps, 'graph.steps').map((item) => {
      const step = expectRecord(item);
      const markers = expectRecord(step.markers);
      return {
        name: expectString(step.name, 'step.name'),
        documentation: expectString(step.documentation, 'step.documentation'),
        span: parseSpan(step.span),
        markers: {
          looped: expectBoolean(markers.looped, 'markers.looped'),
          forked: expectBoolean(markers.forked, 'markers.forked'),
          waits: expectBoolean(markers.waits, 'markers.waits'),
        },
      };
    }),
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

function parseLayout(value: unknown): LayoutRecord {
  const record = expectRecord(value);
  const positionsRecord = expectRecord(record.positions);
  const positions: Record<string, LayoutPosition> = {};
  for (const [name, rawPosition] of Object.entries(positionsRecord)) {
    const position = expectRecord(rawPosition);
    positions[name] = {
      x: expectNumber(position.x, 'position.x'),
      y: expectNumber(position.y, 'position.y'),
    };
  }
  return { positions };
}

function nullableString(value: unknown, field: string): string | null {
  return value === null ? null : expectString(value, field);
}

function expectRecord(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new Error('Invalid authoring response: expected object');
  }
  return value as Record<string, unknown>;
}

function expectArray(value: unknown, field: string): unknown[] {
  if (!Array.isArray(value)) throw new Error(`Invalid authoring response: ${field}`);
  return value;
}

function expectString(value: unknown, field: string): string {
  if (typeof value !== 'string') throw new Error(`Invalid authoring response: ${field}`);
  return value;
}

function expectNumber(value: unknown, field: string): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    throw new Error(`Invalid authoring response: ${field}`);
  }
  return value;
}

function expectBoolean(value: unknown, field: string): boolean {
  if (typeof value !== 'boolean') throw new Error(`Invalid authoring response: ${field}`);
  return value;
}
