export type TextRange = { from: number; to: number };
export type TextDelta = TextRange & { insert: string };
export type PixelPosition = { left: number; top: number; bottom: number };
export type LayoutMetrics = {
  lineHeight: number;
  characterWidth: number;
  contentWidth: number;
  contentHeight: number;
  scrollTop: number;
  scrollLeft: number;
};
// Offsets are UTF-16 code units (web-tree-sitter's native index space — NOT
// bytes; treating them as bytes drags every highlight left past each
// multi-byte character, the operator-reported drift of 2026-07-12).
export type HighlightSpan = { startIndex: number; endIndex: number; capture: string };
export type LineBackgroundDecoration = { line: number; className: string };
export type GutterTextDecoration = { line: number; text: string; className: string };
export type UnderlineDecoration = TextRange & { className: string; message?: string };
export type EditorDecorations = {
  lineBackgrounds?: readonly LineBackgroundDecoration[];
  gutterTexts?: readonly GutterTextDecoration[];
  underlines?: readonly UnderlineDecoration[];
};
export type EditorChange = { content: string; origin: 'local' | 'remote' };
export type MouseHover = { position: number; clientX: number; clientY: number };
export type CursorChange = { position: number };

/** Host-facing text editor contract. AWL-aware code depends only on this seam. */
export interface AuthoringEditor {
  getContent(): string;
  /** Applies all ranges as one undo unit while mapping selection and scroll. */
  applyTextDelta(ranges: readonly TextDelta[], origin?: 'local' | 'remote'): void;
  onChange(listener: (change: EditorChange) => void): () => void;
  positionToPixel(position: number): PixelPosition | null;
  getLayoutMetrics(): LayoutMetrics;
  onScroll(listener: (metrics: LayoutMetrics) => void): () => void;
  setHighlightSpans(spans: readonly HighlightSpan[]): void;
  setDecorations(decorations: EditorDecorations): void;
  onMouseHover(listener: (hover: MouseHover | null) => void): () => void;
  onCursorChange(listener: (change: CursorChange) => void): () => void;
  setCursor(position: number): void;
  focus(): void;
  destroy(): void;
}
