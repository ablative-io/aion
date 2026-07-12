import { describe, expect, test } from 'bun:test';

import { createAuthoringFacade, GestureRefusedError } from './facade';

const fixtures = [
  {
    ok: true,
    deploys_green: true,
    steps: 3,
    diagnostics: [],
    semantic: { entries: [], graph: { steps: [], edges: [], child_calls: [] } },
    expected: {
      ok: true,
      deploysGreen: true,
      steps: 3,
      diagnostics: [],
      semantic: { entries: [], graph: { steps: [], edges: [], childCalls: [] } },
    },
  },
  {
    ok: true,
    deploys_green: false,
    steps: 2,
    diagnostics: [{ class: 'emit_subset', message: 'backend subset', line: 4, column: 2 }],
    semantic: null,
    expected: {
      ok: true,
      deploysGreen: false,
      steps: 2,
      diagnostics: [{ class: 'emit_subset', message: 'backend subset', line: 4, column: 2 }],
      semantic: null,
    },
  },
  {
    ok: false,
    deploys_green: false,
    steps: null,
    diagnostics: [{ class: 'error', message: 'expected step', line: 1, column: 1 }],
    semantic: null,
    expected: {
      ok: false,
      deploysGreen: false,
      steps: null,
      diagnostics: [{ class: 'error', message: 'expected step', line: 1, column: 1 }],
      semantic: null,
    },
  },
];

describe('authoring facade parsing', () => {
  for (const [index, fixture] of fixtures.entries()) {
    test(`parses diagnostic state ${index + 1}`, async () => {
      const facade = createAuthoringFacade(async () => Response.json(fixture));
      expect(JSON.stringify(await facade.check('workflow demo {}'))).toBe(
        JSON.stringify(fixture.expected)
      );
    });
  }

  test('sends the documented check shape', async () => {
    let body = '';
    const facade = createAuthoringFacade(async (_input, init) => {
      body = String(init?.body);
      return Response.json(fixtures[0]);
    });
    await facade.check('source', 'nested/demo.awl');
    expect(JSON.parse(body)).toEqual({ source: 'source', path: 'nested/demo.awl' });
  });

  test('surfaces typed gesture refusals without applying source', async () => {
    const facade = createAuthoringFacade(async () =>
      Response.json({
        ok: false,
        diagnostics: [],
        refusal: { code: 'name_collision', message: 'step name already exists' },
      })
    );
    const operation = { type: 'add_step', name: 'existing' } as const;

    try {
      await facade.edit('source', operation);
      throw new Error('gesture unexpectedly succeeded');
    } catch (error) {
      expect(error).toBeInstanceOf(GestureRefusedError);
      expect((error as GestureRefusedError).code).toBe('name_collision');
      expect((error as Error).message).toBe('step name already exists');
    }
  });
});
