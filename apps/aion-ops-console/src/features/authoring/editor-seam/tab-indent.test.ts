import { expect, test } from 'bun:test';

import { applyTabBindingForTest } from './codemirror';

test('Tab inserts two spaces at the cursor, never a tab character', () => {
  expect(applyTabBindingForTest('greet', { anchor: 0 }, 'Tab')).toBe('  greet');

  // Mid-line cursor: the line indents by one two-space unit; no \t anywhere.
  const midLine = applyTabBindingForTest('step greet', { anchor: 4 }, 'Tab');
  expect(midLine).toBe('  step greet');
  expect(midLine.includes('\t')).toBe(false);
});

test('Tab indents every line of a selection by one two-space unit', () => {
  const content = 'step greet\nsay(name: name)';
  const indented = applyTabBindingForTest(content, { anchor: 0, head: content.length }, 'Tab');
  expect(indented).toBe('  step greet\n  say(name: name)');
  expect(indented.includes('\t')).toBe(false);
});

test('Shift-Tab dedents by one two-space unit', () => {
  expect(applyTabBindingForTest('    say(name: name)', { anchor: 6 }, 'Shift-Tab')).toBe(
    '  say(name: name)'
  );
  expect(applyTabBindingForTest('greet', { anchor: 0 }, 'Shift-Tab')).toBe('greet');
});

test('the Tab binding cannot introduce a tab character into an AWL document', () => {
  // The AWL lexer rejects tabs in indentation outright; sweep both directions
  // over a nested document and assert no \t ever appears.
  let content = 'workflow sample\n  step greet\nsay(name: name)';
  for (const key of ['Tab', 'Tab', 'Shift-Tab', 'Tab'] as const) {
    content = applyTabBindingForTest(content, { anchor: 0, head: content.length }, key);
    expect(content.includes('\t')).toBe(false);
  }
});
