import type { AuthoringEditor } from '../editor-seam/types';
import { minimalTextDelta } from './diagnostics';
import type { EditResult, GestureOperation } from './facade';

export type GestureFacade = {
  edit(source: string, operation: GestureOperation): Promise<EditResult>;
};

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
  editor.applyTextDelta(minimalTextDelta(before, result.source));
  return result;
}
