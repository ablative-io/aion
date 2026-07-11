import { Language, Parser, Query } from 'web-tree-sitter';
import awlWasmUrl from '../assets/awl.wasm?url';
import highlightQuery from '../assets/highlights.scm?raw';

// Vendored from github.com/tomWhiting/tree-sitter-awl at
// fa1afb22c756175131d252380fb4ba9e4b68a321 using tree-sitter-cli 0.26.10.
let parser: Parser | undefined;
let query: Query | undefined;

async function initialize() {
  if (parser !== undefined && query !== undefined) return;
  await Parser.init();
  const language = await Language.load(awlWasmUrl);
  parser = new Parser();
  parser.setLanguage(language);
  query = new Query(language, highlightQuery);
}

self.onmessage = async (event: MessageEvent<{ id: number; source: string }>) => {
  const { id, source } = event.data;
  try {
    await initialize();
    const tree = parser?.parse(source);
    if (tree === undefined || tree === null || query === undefined)
      throw new Error('AWL parser unavailable');
    const spans = query.captures(tree.rootNode).map((capture) => ({
      startByte: capture.node.startIndex,
      endByte: capture.node.endIndex,
      capture: capture.name,
    }));
    tree.delete();
    self.postMessage({ id, spans });
  } catch (error) {
    self.postMessage({ id, error: error instanceof Error ? error.message : 'Highlighting failed' });
  }
};
