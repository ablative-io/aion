import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import { ExpandableRow, resolveExpanded } from './expandable-row';

describe('resolveExpanded', () => {
  test('the controlled prop wins over internal state whenever present', () => {
    expect(resolveExpanded(true, false)).toBe(true);
    expect(resolveExpanded(false, true)).toBe(false);
  });

  test('uncontrolled rows fall back to internal state', () => {
    expect(resolveExpanded(undefined, true)).toBe(true);
    expect(resolveExpanded(undefined, false)).toBe(false);
  });
});

describe('ExpandableRow render states', () => {
  const row = (props: { expanded?: boolean; defaultExpanded?: boolean }) => (
    <ExpandableRow summary="Bash: cargo test" status="running" pips={3} pipsFilled={1} {...props}>
      <pre>stdout goes here</pre>
    </ExpandableRow>
  );

  test('uncontrolled: defaultExpanded drives the initial state', () => {
    expect(renderToStaticMarkup(row({ defaultExpanded: true }))).toContain('data-expanded="true"');
    expect(renderToStaticMarkup(row({}))).toContain('data-expanded="false"');
  });

  test('controlled: the expanded prop overrides defaultExpanded', () => {
    const markup = renderToStaticMarkup(row({ expanded: false, defaultExpanded: true }));
    expect(markup).toContain('data-expanded="false"');
    expect(renderToStaticMarkup(row({ expanded: true }))).toContain('data-expanded="true"');
  });

  test('collapsed summary keeps the one-line anatomy: text, status dot, pips', () => {
    const markup = renderToStaticMarkup(row({}));
    expect(markup).toContain('Bash: cargo test');
    expect(markup).toContain('data-slot="status-dot"');
    expect(markup).toContain('data-slot="expandable-row-pips"');
    expect(markup).toContain('aria-expanded="false"');
  });

  test('detail region is always mounted for measurement but hidden when collapsed', () => {
    const markup = renderToStaticMarkup(row({}));
    expect(markup).toContain('stdout goes here');
    expect(markup).toContain('aria-hidden="true"');
  });
});
