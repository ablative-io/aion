declare module 'dagre' {
  type GraphLabel = {
    width?: number;
    height?: number;
    rankdir?: string;
    nodesep?: number;
    ranksep?: number;
  };
  type Edge = { v: string; w: string; name?: string };
  class Graph {
    setDefaultEdgeLabel(callback: () => Record<string, never>): this;
    setGraph(label: GraphLabel): this;
    setNode(id: string, label: { width: number; height: number }): this;
    setEdge(source: string, target: string): this;
    node(id: string): { x: number; y: number; width: number; height: number };
    edges(): Edge[];
  }
  function layout(graph: Graph): void;
  const dagre: { graphlib: { Graph: typeof Graph }; layout: typeof layout };
  export default dagre;
}
