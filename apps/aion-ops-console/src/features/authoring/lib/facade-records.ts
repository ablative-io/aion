import { expectNumber, expectRecord, expectString } from './facade-values';
import type { LayoutPosition, LayoutRecord } from './projection-types';

export type RenameMapping = { kind: 'step' | 'binding'; from: string; to: string };

export function parseRenameMapping(value: unknown): RenameMapping {
  const record = expectRecord(value);
  const kind = expectString(record.kind, 'rename.kind');
  if (kind !== 'step' && kind !== 'binding') {
    throw new Error('Invalid authoring response: rename.kind');
  }
  return {
    kind,
    from: expectString(record.from, 'rename.from'),
    to: expectString(record.to, 'rename.to'),
  };
}

export function parseLayout(value: unknown): LayoutRecord {
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
