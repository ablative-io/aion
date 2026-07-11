import { expect, test } from 'bun:test';
import { readdir, readFile } from 'node:fs/promises';
import { join, relative } from 'node:path';

const featureRoot = join(import.meta.dir, '..');

test('only the CodeMirror implementation imports @codemirror packages', async () => {
  const violations: string[] = [];
  for (const path of await sourceFiles(featureRoot)) {
    if (path.endsWith('editor-seam/codemirror.ts')) continue;
    const source = await readFile(path, 'utf8');
    if (/from\s+['"]@codemirror\//.test(source) || /import\s*\(['"]@codemirror\//.test(source)) {
      violations.push(relative(featureRoot, path));
    }
  }
  expect(violations).toEqual([]);
});

async function sourceFiles(directory: string): Promise<string[]> {
  const entries = await readdir(directory, { withFileTypes: true });
  const nested = await Promise.all(
    entries.map((entry) => {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) return sourceFiles(path);
      return /\.(ts|tsx)$/.test(entry.name) ? [path] : [];
    })
  );
  return nested.flat();
}
