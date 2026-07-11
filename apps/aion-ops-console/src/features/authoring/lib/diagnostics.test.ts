import { describe, expect, test } from 'bun:test';

import { mapDiagnostics, minimalTextDelta, statusForCheck } from './diagnostics';

describe('authoring diagnostic mapping', () => {
  test('keeps error and emit-subset warning decorations visually distinct', () => {
    const mapped = mapDiagnostics('first\nsecond', [
      { class: 'error', message: 'bad', line: 1, column: 2 },
      { class: 'emit_subset', message: 'not emitted', line: 2, column: 1 },
    ]);
    expect(mapped.underlines?.map((item) => item.className)).toEqual([
      'awl-underline-error',
      'awl-underline-warning',
    ]);
    expect(mapped.gutterTexts?.[1]?.className).toBe('awl-gutter-warning');
    expect(mapped.underlines?.[1]?.from).toBe(6);
  });

  test('rolls up all three honest states', () => {
    expect(
      statusForCheck({ ok: true, deploysGreen: true, steps: 1, diagnostics: [], semantic: null })
    ).toEqual({
      tone: 'green',
      label: 'deploys green',
    });
    expect(
      statusForCheck({
        ok: true,
        deploysGreen: false,
        steps: 1,
        diagnostics: [{ class: 'emit_subset', message: 'x', line: 1, column: 1 }],
        semantic: null,
      }).label
    ).toBe("checks, won't deploy yet: 1");
    expect(
      statusForCheck({
        ok: false,
        deploysGreen: false,
        steps: 0,
        diagnostics: [{ class: 'error', message: 'x', line: 1, column: 1 }],
        semantic: null,
      }).label
    ).toBe('1 errors');
  });

  test('format computes one minimal replacement', () => {
    expect(minimalTextDelta('hello wrld', 'hello world')).toEqual([
      { from: 7, to: 7, insert: 'o' },
    ]);
  });
});
