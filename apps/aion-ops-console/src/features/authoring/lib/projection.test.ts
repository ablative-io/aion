import { describe, expect, test } from 'bun:test';

import {
  byteOffsetToUtf16,
  diagnosticsByStep,
  stepAtPosition,
  utf16ToByteOffset,
} from './projection';
import type { GraphProjection } from './projection-types';

const plain = {
  kind: 'plain' as const,
  region: null,
  distribution: null,
  collect: null,
  subflow: null,
  substeps: null,
  visits: null,
  decision: false,
  waits: false,
};

const graph: GraphProjection = {
  steps: [
    {
      name: 'first',
      documentation: 'First',
      activities: [],
      span: { start: 0, end: 5, line: 1, column: 1 },
      ...plain,
    },
    {
      name: 'second',
      documentation: 'Second',
      activities: [],
      span: { start: 12, end: 18, line: 4, column: 1 },
      ...plain,
      kind: 'distribute',
      distribution: { binding: 'item', collection: 'state.items' },
    },
  ],
  edges: [],
  childCalls: [],
};

describe('canvas projection span lookup', () => {
  test('maps UTF-8 semantic bytes to editor UTF-16 positions', () => {
    const source = 'é😀 second';
    for (const byte of [0, 2, 6, 7, 13]) {
      expect(utf16ToByteOffset(source, byteOffsetToUtf16(source, byte))).toBe(byte);
    }
  });

  test('selects the containing step from ordered semantic spans', () => {
    expect(stepAtPosition(graph, 'first body\nsecond body', 3)?.name).toBe('first');
    expect(stepAtPosition(graph, 'first body\nsecond body', 14)?.name).toBe('second');
  });

  test('distinguishes primary diagnostics from unreachable cascades', () => {
    const tones = diagnosticsByStep(graph, [
      { class: 'error', message: 'unknown binding', line: 2, column: 1 },
      { class: 'error', message: 'step is unreachable', line: 5, column: 1 },
    ]);
    expect(tones.get('first')).toBe('primary');
    expect(tones.get('second')).toBe('cascade');
  });
});
