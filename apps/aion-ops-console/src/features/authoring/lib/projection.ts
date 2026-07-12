import type { AwlDiagnostic } from './facade';
import type { GraphProjection, ProjectionStep } from './projection-types';

export type CanvasDiagnosticTone = 'primary' | 'cascade';

export function byteOffsetToUtf16(source: string, byteOffset: number): number {
  const bytes = new TextEncoder().encode(source);
  const prefix = bytes.slice(0, Math.min(Math.max(byteOffset, 0), bytes.length));
  return new TextDecoder().decode(prefix).length;
}

export function utf16ToByteOffset(source: string, position: number): number {
  const clamped = Math.min(Math.max(position, 0), source.length);
  return new TextEncoder().encode(source.slice(0, clamped)).length;
}

export function stepAtPosition(
  graph: GraphProjection,
  source: string,
  utf16Position: number
): ProjectionStep | null {
  const bytePosition = utf16ToByteOffset(source, utf16Position);
  const ordered = [...graph.steps].sort((left, right) => left.span.start - right.span.start);
  for (let index = ordered.length - 1; index >= 0; index -= 1) {
    const step = ordered[index];
    if (step !== undefined && bytePosition >= step.span.start) return step;
  }
  return null;
}

export function diagnosticsByStep(
  graph: GraphProjection,
  diagnostics: readonly AwlDiagnostic[]
): ReadonlyMap<string, CanvasDiagnosticTone> {
  const ordered = [...graph.steps].sort((left, right) => left.span.line - right.span.line);
  const tones = new Map<string, CanvasDiagnosticTone>();
  for (const diagnostic of diagnostics) {
    if (diagnostic.class !== 'error') continue;
    const step = stepAtLine(ordered, diagnostic.line);
    if (step === null) continue;
    const cascade = diagnostic.message.toLowerCase().includes('unreachable');
    const existing = tones.get(step.name);
    if (existing === undefined || (existing === 'cascade' && !cascade)) {
      tones.set(step.name, cascade ? 'cascade' : 'primary');
    }
  }
  return tones;
}

function stepAtLine(ordered: readonly ProjectionStep[], line: number): ProjectionStep | null {
  for (let index = ordered.length - 1; index >= 0; index -= 1) {
    const step = ordered[index];
    if (step !== undefined && line >= step.span.line) return step;
  }
  return null;
}
