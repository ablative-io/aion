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
export type HighlightSpan = { startByte: number; endByte: number; capture: string };
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
  focus(): void;
  destroy(): void;
}
