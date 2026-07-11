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
  steps: number;
  diagnostics: AwlDiagnostic[];
  semantic: object | null;
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
        steps: expectNumber(value.steps, 'steps'),
        diagnostics: expectArray(value.diagnostics, 'diagnostics').map(parseDiagnostic),
        semantic: value.semantic === null ? null : expectRecord(value.semantic),
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
