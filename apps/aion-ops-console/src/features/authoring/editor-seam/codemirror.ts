import {
  history,
  historyKeymap,
  indentLess,
  indentMore,
  indentWithTab,
} from '@codemirror/commands';
import { indentUnit } from '@codemirror/language';
import {
  EditorState,
  RangeSet,
  type StateCommand,
  StateEffect,
  StateField,
} from '@codemirror/state';
import {
  Decoration,
  type DecorationSet,
  EditorView,
  GutterMarker,
  gutter,
  keymap,
  ViewPlugin,
  type ViewUpdate,
} from '@codemirror/view';

import type {
  AuthoringEditor,
  CursorChange,
  EditorChange,
  EditorDecorations,
  HighlightSpan,
  LayoutMetrics,
  MouseHover,
  TextDelta,
} from './types';

const setHighlights = StateEffect.define<DecorationSet>();
const setHostDecorations = StateEffect.define<DecorationSet>();
const setGutters = StateEffect.define<RangeSet<GutterMarker>>();
const remoteOrigin = StateEffect.define<boolean>();

const highlightsField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update: (value, transaction) => {
    let next = value.map(transaction.changes);
    for (const effect of transaction.effects) if (effect.is(setHighlights)) next = effect.value;
    return next;
  },
  provide: (field) => EditorView.decorations.from(field),
});
const hostDecorationsField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update: (value, transaction) => {
    let next = value.map(transaction.changes);
    for (const effect of transaction.effects)
      if (effect.is(setHostDecorations)) next = effect.value;
    return next;
  },
  provide: (field) => EditorView.decorations.from(field),
});
const guttersField = StateField.define<RangeSet<GutterMarker>>({
  create: () => RangeSet.empty,
  update: (value, transaction) => {
    for (const effect of transaction.effects) if (effect.is(setGutters)) return effect.value;
    return value.map(transaction.changes);
  },
});

/**
 * AWL indentation is two SPACES per level — the lexer rejects tab characters
 * in indentation outright ("tabs are not allowed in indentation"), so the
 * indent unit must never be a tab. `indentWithTab` binds Tab/Shift-Tab to
 * indent-more/indent-less, which insert/remove this unit. The accessibility
 * escape hatch CodeMirror documents is untouched: it lives in the view's
 * keydown path (Escape arms tab-focus mode for two seconds, during which Tab
 * bypasses keymaps and moves focus), ahead of this binding.
 */
const AWL_INDENT_UNIT = '  ';
const indentation = [indentUnit.of(AWL_INDENT_UNIT), keymap.of([indentWithTab])];

/**
 * Run one side of the Tab binding over a headless document, exported so the
 * seam tests can pin the indentation contract without a DOM. `indentWithTab`
 * binds Tab to `indentMore` and Shift-Tab to `indentLess`; these are the
 * same command objects the editor installs, run against the same indent-unit
 * facet value.
 */
export function applyTabBindingForTest(
  content: string,
  selection: { anchor: number; head?: number },
  key: 'Tab' | 'Shift-Tab'
): string {
  let state = EditorState.create({
    doc: content,
    selection,
    extensions: [indentUnit.of(AWL_INDENT_UNIT)],
  });
  const command: StateCommand = key === 'Tab' ? indentMore : indentLess;
  command({
    state,
    dispatch: (transaction) => {
      state = transaction.state;
    },
  });
  return state.doc.toString();
}

class TextMarker extends GutterMarker {
  constructor(
    readonly text: string,
    readonly markerClass: string
  ) {
    super();
  }
  override toDOM() {
    const element = document.createElement('span');
    element.className = this.markerClass;
    element.textContent = this.text;
    return element;
  }
}

/** The only CodeMirror-aware implementation of the editor seam. */
export function createCodeMirrorEditor(
  parent: HTMLElement,
  initialContent: string
): AuthoringEditor {
  const changeListeners = new Set<(change: EditorChange) => void>();
  const cursorListeners = new Set<(change: CursorChange) => void>();
  const scrollListeners = new Set<(metrics: LayoutMetrics) => void>();
  const hoverListeners = new Set<(hover: MouseHover | null) => void>();

  const view = new EditorView({
    parent,
    state: EditorState.create({
      doc: initialContent,
      extensions: [
        history(),
        keymap.of(historyKeymap),
        indentation,
        highlightsField,
        hostDecorationsField,
        guttersField,
        gutter({
          class: 'authoring-diagnostic-gutter',
          markers: (editor) => editor.state.field(guttersField),
        }),
        EditorView.lineWrapping,
        EditorView.theme({
          '&': {
            height: '100%',
            backgroundColor: 'var(--surface-elevated)',
            color: 'var(--foreground)',
            fontSize: '0.8125rem',
          },
          '.cm-scroller': { lineHeight: '1.65' },
          '.cm-content': {
            padding: '0.75rem 0',
            fontFamily: 'var(--font-mono)',
            caretColor: 'var(--accent-primary)',
          },
          '.cm-line': { padding: '0 1rem' },
          '.cm-cursor': { borderLeftColor: 'var(--foreground)' },
          '.cm-gutters': {
            backgroundColor: 'var(--surface-base)',
            color: 'var(--muted-foreground)',
            borderRightColor: 'var(--border)',
          },
          '.cm-gutterElement': { padding: '0 0.625rem' },
          '.cm-activeLine, .cm-activeLineGutter': { backgroundColor: 'var(--accent)' },
          '&.cm-focused .cm-selectionBackground, ::selection': {
            backgroundColor: 'var(--secondary)',
          },
        }),
        EditorView.updateListener.of((update) =>
          notifyEditorUpdate(update, changeListeners, cursorListeners)
        ),
        EditorView.domEventHandlers({
          mousemove(event, editor) {
            const position = editor.posAtCoords({ x: event.clientX, y: event.clientY });
            const hover: MouseHover | null =
              position === null
                ? null
                : { position, clientX: event.clientX, clientY: event.clientY };
            for (const listener of hoverListeners) listener(hover);
          },
          mouseleave() {
            for (const listener of hoverListeners) listener(null);
          },
        }),
        ViewPlugin.fromClass(
          class {
            constructor(readonly editor: EditorView) {}
            update(update: ViewUpdate) {
              if (update.viewportChanged || update.geometryChanged) {
                const metrics = readMetrics(update.view);
                for (const listener of scrollListeners) listener(metrics);
              }
            }
          }
        ),
      ],
    }),
  });

  return {
    getContent: () => view.state.doc.toString(),
    applyTextDelta(ranges: readonly TextDelta[], origin = 'local') {
      if (ranges.length === 0) return;
      const scrollTop = view.scrollDOM.scrollTop;
      const scrollLeft = view.scrollDOM.scrollLeft;
      const effects = origin === 'remote' ? [remoteOrigin.of(true)] : [];
      view.dispatch({
        changes: ranges.map(({ from, to, insert }) => ({ from, to, insert })),
        effects,
        scrollIntoView: false,
      });
      view.scrollDOM.scrollTo({ top: scrollTop, left: scrollLeft });
    },
    onChange(listener) {
      changeListeners.add(listener);
      return () => changeListeners.delete(listener);
    },
    positionToPixel(position) {
      const coordinates = view.coordsAtPos(position);
      const root = view.dom.getBoundingClientRect();
      return coordinates === null
        ? null
        : {
            left: coordinates.left - root.left,
            top: coordinates.top - root.top,
            bottom: coordinates.bottom - root.top,
          };
    },
    getLayoutMetrics: () => readMetrics(view),
    onScroll(listener) {
      scrollListeners.add(listener);
      return () => scrollListeners.delete(listener);
    },
    setHighlightSpans(spans) {
      view.dispatch({ effects: setHighlights.of(highlightDecorations(view, spans)) });
    },
    setDecorations(decorations) {
      view.dispatch({
        effects: [
          setHostDecorations.of(hostDecorationSet(view, decorations)),
          setGutters.of(gutterMarkers(decorations, view)),
        ],
      });
    },
    onMouseHover(listener) {
      hoverListeners.add(listener);
      return () => hoverListeners.delete(listener);
    },
    onCursorChange(listener) {
      cursorListeners.add(listener);
      return () => cursorListeners.delete(listener);
    },
    setCursor(position) {
      const anchor = Math.min(Math.max(position, 0), view.state.doc.length);
      view.dispatch({ selection: { anchor }, scrollIntoView: true });
    },
    focus: () => view.focus(),
    destroy: () => view.destroy(),
  };
}

function notifyEditorUpdate(
  update: ViewUpdate,
  changeListeners: ReadonlySet<(change: EditorChange) => void>,
  cursorListeners: ReadonlySet<(change: CursorChange) => void>
) {
  if (update.docChanged) notifyDocumentChange(update, changeListeners);
  if (update.selectionSet || update.docChanged) {
    const change: CursorChange = { position: update.state.selection.main.head };
    for (const listener of cursorListeners) listener(change);
  }
}

function notifyDocumentChange(
  update: ViewUpdate,
  listeners: ReadonlySet<(change: EditorChange) => void>
) {
  const remote = update.transactions.some((transaction) =>
    transaction.effects.some((effect) => effect.is(remoteOrigin))
  );
  if (remote) return;
  const change: EditorChange = {
    content: update.state.doc.toString(),
    origin: 'local',
  };
  for (const listener of listeners) listener(change);
}

function highlightDecorations(view: EditorView, spans: readonly HighlightSpan[]): DecorationSet {
  // Span offsets are UTF-16 code units — CM6's own index space. Clamp to the
  // live document: spans race edits, and the next parse reconciles.
  const docLength = view.state.doc.length;
  const ranges = spans.flatMap((span) => {
    const from = Math.min(span.startIndex, docLength);
    const to = Math.min(span.endIndex, docLength);
    return from < to
      ? [Decoration.mark({ class: `awl-syntax-${captureClass(span.capture)}` }).range(from, to)]
      : [];
  });
  return Decoration.set(ranges, true);
}

function hostDecorationSet(view: EditorView, decorations: EditorDecorations): DecorationSet {
  const ranges = [];
  for (const item of decorations.lineBackgrounds ?? []) {
    const line = view.state.doc.line(Math.min(Math.max(item.line, 1), view.state.doc.lines));
    ranges.push(Decoration.line({ class: item.className }).range(line.from));
  }
  for (const item of decorations.underlines ?? []) {
    const from = Math.min(item.from, view.state.doc.length);
    const to = Math.min(item.to, view.state.doc.length);
    if (from < to)
      ranges.push(
        Decoration.mark({ class: item.className, attributes: { title: item.message ?? '' } }).range(
          from,
          to
        )
      );
  }
  return Decoration.set(ranges, true);
}

function gutterMarkers(decorations: EditorDecorations, view: EditorView): RangeSet<GutterMarker> {
  const markers = [];
  for (const item of decorations.gutterTexts ?? []) {
    const line = view.state.doc.line(Math.min(Math.max(item.line, 1), view.state.doc.lines));
    markers.push(new TextMarker(item.text, item.className).range(line.from));
  }
  return RangeSet.of(markers, true);
}

function readMetrics(view: EditorView): LayoutMetrics {
  return {
    lineHeight: view.defaultLineHeight,
    characterWidth: view.defaultCharacterWidth,
    contentWidth: view.contentDOM.scrollWidth,
    contentHeight: view.contentDOM.scrollHeight,
    scrollTop: view.scrollDOM.scrollTop,
    scrollLeft: view.scrollDOM.scrollLeft,
  };
}

function captureClass(capture: string): string {
  return capture.replaceAll('.', '-').replaceAll(/[^a-zA-Z0-9_-]/g, '');
}
