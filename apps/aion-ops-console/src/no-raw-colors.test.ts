import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative } from 'node:path';

// Design-language guard: components never hardcode palette colors — every color
// comes from the semantic tokens in src/index.css, the single source of truth.
// This bans raw color literals (6/8-digit hex, rgb/rgba/hsl/hsla/oklch) in all
// non-test TS/TSX source, INCLUDING var() fallbacks: duplicated fallbacks are
// exactly how the palette drifted from the tokens once already.
// 3/4-digit hexes are deliberately not matched — issue refs like "#164" in
// comments are hex-shaped and would false-positive.
const RAW_COLOR = /#[0-9a-fA-F]{6}(?:[0-9a-fA-F]{2})?(?![0-9a-fA-F])|\b(?:rgba?|hsla?|oklch)\(/;

const SRC_ROOT = import.meta.dir;

function isExcluded(path: string): boolean {
  return path.includes('__tests__') || path.endsWith('.test.ts') || path.endsWith('.test.tsx');
}

function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      out.push(...sourceFiles(path));
    } else if (/\.tsx?$/.test(entry.name) && !isExcluded(path)) {
      out.push(path);
    }
  }
  return out;
}

describe('token discipline', () => {
  test('no raw color literals outside index.css', () => {
    const offenders: string[] = [];
    for (const file of sourceFiles(SRC_ROOT)) {
      const lines = readFileSync(file, 'utf8').split('\n');
      lines.forEach((line, i) => {
        if (RAW_COLOR.test(line)) {
          offenders.push(`${relative(SRC_ROOT, file)}:${i + 1}: ${line.trim()}`);
        }
      });
    }
    expect(offenders).toEqual([]);
  });
});
