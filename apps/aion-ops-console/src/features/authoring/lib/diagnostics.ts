import type { EditorDecorations } from '../editor-seam/types';
import type { AwlDiagnostic, CheckResult } from './facade';

export type AuthoringStatus =
  | { tone: 'green'; label: 'deploys green' }
  | { tone: 'amber'; label: string }
  | { tone: 'red'; label: string };

export function mapDiagnostics(
  source: string,
  diagnostics: readonly AwlDiagnostic[]
): EditorDecorations {
  const lines = lineStarts(source);
  return {
    lineBackgrounds: diagnostics.map((diagnostic) => ({
      line: diagnostic.line,
      className: diagnostic.class === 'error' ? 'awl-line-error' : 'awl-line-warning',
    })),
    gutterTexts: diagnostics.map((diagnostic) => ({
      line: diagnostic.line,
      text: diagnostic.class === 'error' ? '!' : '▲',
      className: diagnostic.class === 'error' ? 'awl-gutter-error' : 'awl-gutter-warning',
    })),
    underlines: diagnostics.map((diagnostic) => {
      const from = offsetAt(lines, source.length, diagnostic.line, diagnostic.column);
      return {
        from,
        to: Math.min(source.length, from + Math.max(codePointWidth(source, from), 1)),
        className: diagnostic.class === 'error' ? 'awl-underline-error' : 'awl-underline-warning',
        message: diagnostic.message,
      };
    }),
  };
}

export function statusForCheck(result: CheckResult): AuthoringStatus {
  const errors = result.diagnostics.filter((diagnostic) => diagnostic.class === 'error').length;
  if (!result.ok || errors > 0) return { tone: 'red', label: `${errors} errors` };
  const warnings = result.diagnostics.filter(
    (diagnostic) => diagnostic.class === 'emit_subset'
  ).length;
  if (!result.deploysGreen)
    return { tone: 'amber', label: `checks, won't deploy yet: ${warnings}` };
  return { tone: 'green', label: 'deploys green' };
}

export function minimalTextDelta(before: string, after: string) {
  if (before === after) return [];
  let start = 0;
  while (start < before.length && start < after.length && before[start] === after[start])
    start += 1;
  let beforeEnd = before.length;
  let afterEnd = after.length;
  while (beforeEnd > start && afterEnd > start && before[beforeEnd - 1] === after[afterEnd - 1]) {
    beforeEnd -= 1;
    afterEnd -= 1;
  }
  return [{ from: start, to: beforeEnd, insert: after.slice(start, afterEnd) }];
}

function lineStarts(source: string) {
  const starts = [0];
  for (let index = 0; index < source.length; index += 1)
    if (source[index] === '\n') starts.push(index + 1);
  return starts;
}

function offsetAt(starts: number[], length: number, line: number, column: number) {
  const lineStart = starts[Math.min(Math.max(line - 1, 0), starts.length - 1)] ?? 0;
  return Math.min(length, lineStart + Math.max(column - 1, 0));
}

function codePointWidth(source: string, offset: number) {
  return source.codePointAt(offset) !== undefined && (source.codePointAt(offset) ?? 0) > 0xffff
    ? 2
    : 1;
}
