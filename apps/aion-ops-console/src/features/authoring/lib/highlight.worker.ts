import { Language, Parser, Query } from 'web-tree-sitter';
import awlWasmUrl from '../assets/awl.wasm?url';
import highlightQuery from '../assets/highlights.scm?raw';

// Vendored from this repo's tools/tree-sitter-awl (B1+B2 flow-vocab grammar,
// commit 20f4309b) using tree-sitter-cli 0.26.10. The wasm and highlights.scm
// in ../assets must always be replaced together. Mirror push to
// github.com/tomWhiting/tree-sitter-awl is owed after these land.
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
    // web-tree-sitter indexes are UTF-16 code units — pass through untouched.
    const spans = query.captures(tree.rootNode).map((capture) => ({
      startIndex: capture.node.startIndex,
      endIndex: capture.node.endIndex,
      capture: capture.name,
    }));
    tree.delete();
    self.postMessage({ id, spans });
  } catch (error) {
    self.postMessage({ id, error: error instanceof Error ? error.message : 'Highlighting failed' });
  }
};
