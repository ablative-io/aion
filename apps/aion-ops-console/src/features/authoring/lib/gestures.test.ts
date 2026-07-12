import { expect, test } from 'bun:test';

import type {
  AuthoringEditor,
  EditorChange,
  EditorDecorations,
  HighlightSpan,
  LayoutMetrics,
  MouseHover,
  TextDelta,
} from '../editor-seam/types';
import { createAuthoringFacade } from './facade';
import { applyGestureThroughSeam, DocumentChangedDuringGestureError } from './gestures';

test('gesture travels through facade boundary and one editor-seam delta', async () => {
  const before = '//! Demo.\nworkflow demo\n  outcome done: type String, route success\n';
  const after = `${before}\n/// Collect input.\nstep collect\n`;
  let request: unknown;
  const facade = createAuthoringFacade(async (input, init) => {
    request = { input: String(input), body: JSON.parse(String(init?.body)) };
    return Response.json({ ok: true, source: after, diagnostics: [], rename: undefined });
  });
  const applied: (readonly TextDelta[])[] = [];
  const editor = fakeEditor(before, (ranges) => applied.push(ranges));
  const operation = { type: 'add_step', name: 'collect', prose: 'Collect input.' } as const;

  const result = await applyGestureThroughSeam(editor, facade, operation);

  expect(request).toEqual({ input: '/awl/edit', body: { source: before, operation } });
  expect(result.source).toBe(after);
  expect(applied).toHaveLength(1);
  expect(applied[0]).toEqual([
    { from: before.length, to: before.length, insert: after.slice(before.length) },
  ]);
});

test('gesture refuses to apply when the document changed mid-flight', async () => {
  const before = '//! Demo.\nworkflow demo\n  outcome done: type String, route success\n';
  const after = `${before}\nstep collect\n`;
  const facade = createAuthoringFacade(async () =>
    Response.json({ ok: true, source: after, diagnostics: [], rename: undefined })
  );
  const applied: (readonly TextDelta[])[] = [];
  const editor = fakeEditor(before, (ranges) => applied.push(ranges));
  // Simulate a keystroke landing during the server round-trip: the second
  // getContent read (the applying act) must see the drift and refuse.
  let reads = 0;
  editor.getContent = () => {
    reads += 1;
    return reads === 1 ? before : `${before} `;
  };

  await expect(
    applyGestureThroughSeam(editor, facade, {
      type: 'add_step',
      name: 'collect',
      prose: '',
    } as const)
  ).rejects.toBeInstanceOf(DocumentChangedDuringGestureError);
  expect(applied).toHaveLength(0);
});

function fakeEditor(
  content: string,
  apply: (ranges: readonly TextDelta[]) => void
): AuthoringEditor {
  const unsubscribe = () => undefined;
  const metrics: LayoutMetrics = {
    lineHeight: 16,
    characterWidth: 8,
    contentWidth: 0,
    contentHeight: 0,
    scrollTop: 0,
    scrollLeft: 0,
  };
  return {
    getContent: () => content,
    applyTextDelta: apply,
    onChange: (_listener: (change: EditorChange) => void) => unsubscribe,
    positionToPixel: () => null,
    getLayoutMetrics: () => metrics,
    onScroll: (_listener: (value: LayoutMetrics) => void) => unsubscribe,
    setHighlightSpans: (_spans: readonly HighlightSpan[]) => undefined,
    setDecorations: (_decorations: EditorDecorations) => undefined,
    onMouseHover: (_listener: (hover: MouseHover | null) => void) => unsubscribe,
    onCursorChange: () => unsubscribe,
    setCursor: () => undefined,
    focus: () => undefined,
    destroy: () => undefined,
  };
}
