import type { AuthoringEditor } from '../editor-seam/types';
import { minimalTextDelta } from './diagnostics';
import type { EditResult, GestureOperation } from './facade';

export type GestureFacade = {
  edit(source: string, operation: GestureOperation): Promise<EditResult>;
};

export class DocumentChangedDuringGestureError extends Error {
  constructor() {
    super('The document changed while the gesture was in flight; nothing was applied.');
    this.name = 'DocumentChangedDuringGestureError';
  }
}

/**
 * The sole client gesture application path: ask the server to mutate and print
 * the AST, then apply its canonical source through one editor-seam delta call.
 */
export async function applyGestureThroughSeam(
  editor: AuthoringEditor,
  facade: GestureFacade,
  operation: GestureOperation
): Promise<EditResult> {
  const before = editor.getContent();
  const result = await facade.edit(before, operation);
  // The delta's offsets are only valid against the content the server edited.
  // In split view the text editor stays live during the round-trip, so the
  // guard must sit here, at the applying act — not at capture time.
  if (editor.getContent() !== before) throw new DocumentChangedDuringGestureError();
  editor.applyTextDelta(minimalTextDelta(before, result.source));
  return result;
}
