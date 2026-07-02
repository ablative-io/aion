import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative } from 'node:path';

// Token discipline: the vocabulary in src/index.css is the ONLY place color
// values live. This test scans every source file and fails with file:line
// pointers on raw color literals and banned Tailwind palette classes, so the
// vocabulary stays self-enforcing instead of decaying into per-component hex.
//
// A genuinely un-tokenizable line can opt out with an inline comment on the
// SAME line: /* token-exempt: <reason> */ — the reason is mandatory prose.

const SRC_ROOT = join(import.meta.dir, '..');

const SCANNED_EXTENSIONS = ['.ts', '.tsx', '.css'];

// index.css is the token source of truth; generated types are not ours to
// police; tests may build literal fixtures (this file among them).
function isExcluded(relPath: string): boolean {
  return (
    relPath === 'index.css' ||
    relPath.startsWith('types/generated/') ||
    relPath.includes('__tests__/') ||
    /\.test\.(ts|tsx)$/.test(relPath)
  );
}

// Raw color literals. Hex must be a valid color width (3/4/6/8 digits);
// rgb()/rgba()/hsl()/oklch() only when the first argument is numeric, so
// wrappers like rgb(var(--x)) stay legal.
const HEX_COLOR = /#(?:[0-9a-fA-F]{8}|[0-9a-fA-F]{6}|[0-9a-fA-F]{3,4})\b/g;
const FUNCTIONAL_COLOR = /\b(?:rgba?|hsla?|oklch|oklab)\(\s*[\d.]/g;

// Tailwind stock-palette classes (any numeric shade) — the semantic
// utilities (bg-live, text-success, border-danger, bg-surface-*) are the
// vocabulary; the stock palette is banned wholesale.
const BANNED_PALETTE =
  /\b(?:cyan|sky|blue|indigo|violet|purple|fuchsia|pink|rose|red|orange|amber|yellow|lime|green|emerald|teal|zinc|slate|gray|stone|neutral)-\d{2,3}\b/g;

const EXEMPT_MARKER = 'token-exempt:';

type Violation = { file: string; line: number; match: string };

function* walk(dir: string): Generator<string> {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) {
      yield* walk(full);
    } else if (SCANNED_EXTENSIONS.some((ext) => entry.name.endsWith(ext))) {
      yield full;
    }
  }
}

// Blank out comment spans line-by-line (colors in comments — issue refs like
// "#164", token prose — are not violations). Tracks /* */ across lines; a
// "//" starts a line comment unless preceded by ":" (protects https:// in
// string literals). Misclassifying an exotic case can only hide a finding on
// that one line, never invent one.
function stripLine(line: string, state: { inBlock: boolean }): string {
  let out = '';
  let i = 0;
  while (i < line.length) {
    if (state.inBlock) {
      const end = line.indexOf('*/', i);
      if (end === -1) return out;
      state.inBlock = false;
      i = end + 2;
    } else if (line.startsWith('/*', i)) {
      state.inBlock = true;
      i += 2;
    } else if (line.startsWith('//', i) && line[i - 1] !== ':') {
      return out;
    } else {
      out += line[i];
      i += 1;
    }
  }
  return out;
}

export function stripComments(lines: string[]): string[] {
  const state = { inBlock: false };
  return lines.map((line) => stripLine(line, state));
}

export function scanSource(relPath: string, source: string): Violation[] {
  const rawLines = source.split('\n');
  const codeLines = stripComments(rawLines);
  const violations: Violation[] = [];
  codeLines.forEach((code, index) => {
    const raw = rawLines[index] ?? '';
    if (raw.includes(EXEMPT_MARKER)) return;
    for (const pattern of [HEX_COLOR, FUNCTIONAL_COLOR, BANNED_PALETTE]) {
      for (const found of code.matchAll(pattern)) {
        violations.push({ file: relPath, line: index + 1, match: found[0] });
      }
    }
  });
  return violations;
}

describe('token discipline', () => {
  test('no raw color literals or stock-palette classes outside index.css', () => {
    const violations: Violation[] = [];
    for (const path of walk(SRC_ROOT)) {
      const relPath = relative(SRC_ROOT, path);
      if (isExcluded(relPath)) continue;
      violations.push(...scanSource(relPath, readFileSync(path, 'utf8')));
    }
    if (violations.length > 0) {
      const listing = violations.map((v) => `  src/${v.file}:${v.line}  ${v.match}`).join('\n');
      throw new Error(
        `${violations.length} token-discipline violation(s) — use the semantic tokens/utilities from src/index.css, or (rarely) append /* token-exempt: <reason> */ on the line:\n${listing}`
      );
    }
  });

  // Guard the scanner itself: if a regex rots, this fails before the sweep
  // silently starts passing on real violations.
  test('the scanner catches each violation class', () => {
    const bad = [
      "const c = '#d4845a';",
      "const c = '#fff';",
      "const glow = 'rgba(248, 113, 113, 0.12)';",
      "const glass = 'oklch(0.16 0.01 280 / 0.85)';",
      'const cls = "text-cyan-400 bg-zinc-900";',
    ];
    for (const [i, line] of bad.entries()) {
      expect(scanSource('fixture.tsx', line).length).toBeGreaterThanOrEqual(i === 4 ? 2 : 1);
    }
  });

  test('comments, exemptions, and token wrappers stay legal', () => {
    const good = [
      '// enforced as of #164 (see #200)',
      '/* the doc value is #d4845a */',
      "const c = 'var(--accent-primary)';",
      "const wrapped = 'rgb(var(--status-danger))';",
      "const url = 'https://example.invalid/path';",
      "const legacy = '#0f0f14'; /* token-exempt: fixture for this very test */",
    ];
    for (const line of good) {
      expect(scanSource('fixture.tsx', line)).toEqual([]);
    }
  });
});
