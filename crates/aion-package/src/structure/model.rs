//! The workflow graph model: nodes, edges, and per-node correlation identity.
//!
//! A [`WorkflowGraph`] is a faithful projection of the typed Gleam source — a
//! consumer (the dashboard canvas, RM-007) renders it and overlays a run's
//! recorded events by matching each event onto a node's [`CorrelationKey`]. The
//! model is never the authoritative artifact; the typed source is the single
//! source of truth (ADR-014, CN6).

use std::collections::BTreeSet;

/// Stable identifier for a node within one extracted graph.
///
/// The id is the node's deterministic position in the workflow's call order
/// (source order of the recognised primitive calls), which is the order replay
/// re-executes them in. It is opaque to consumers beyond identity and ordering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub usize);

/// The workflow primitive a node represents.
///
/// This is the fixed, known vocabulary of `aion/workflow` (the only surface the
/// extractor understands), plus `Branch` for `case` control flow. Adding a
/// primitive to the SDK requires adding it here; an unrecognised call is never
/// silently dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodePrimitive {
    /// `workflow.run` — a single recorded activity dispatch.
    Run,
    /// `workflow.all` — concurrent fan-out, all must succeed.
    All,
    /// `workflow.race` — concurrent fan-out, first to settle wins.
    Race,
    /// `workflow.map` — concurrent map over a list of activities.
    Map,
    /// `workflow.spawn` — start a child workflow without waiting.
    Spawn,
    /// `workflow.spawn_and_wait` — start a child workflow and await it.
    SpawnAndWait,
    /// `workflow.receive` — await a signal.
    Receive,
    /// `workflow.sleep` — a durable timer the workflow blocks on.
    Sleep,
    /// `workflow.start_timer` — arm a named durable timer.
    StartTimer,
    /// `workflow.cancel_timer` — cancel a previously armed timer.
    CancelTimer,
    /// A `case` expression: workflow control-flow branching.
    Branch,
}

/// The per-node identity a consumer maps recorded events onto.
///
/// The four event-bearing kinds mirror how AD/AT sequences recorded events:
/// activities by their deterministic call order plus name, signals by name,
/// timers by id, and children by spawn ordinal plus name. `ControlFlow` carries
/// no recorded event of its own; it positions a structural node (a branch) in
/// the call order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CorrelationKey {
    /// An activity dispatch: its deterministic call ordinal and activity name.
    ActivitySequence {
        /// Zero-based position of this activity among all activity dispatches,
        /// in workflow call order — the order replay re-applies recorded
        /// `ActivityCompleted` events.
        ordinal: usize,
        /// The engine-facing activity name (the `run` node's activity).
        activity: String,
    },
    /// A signal receive, identified by the signal-reference expression text.
    ///
    /// The signal name is not a string literal at the `receive` call site (it
    /// is a typed `SignalRef`), so the reference identifier is carried as the
    /// stable correlation token a consumer aligns against the run's signal
    /// reference.
    Signal {
        /// The signal-reference expression text passed to `receive`.
        reference: String,
    },
    /// A timer, identified by its literal name as written at the call site.
    Timer {
        /// The timer name string literal (`start_timer`'s first argument).
        id: String,
    },
    /// A child spawn: its deterministic spawn ordinal and the literal child
    /// workflow name.
    Child {
        /// Zero-based position of this spawn among all child spawns, in call
        /// order — the ordinal a recorded child event carries.
        ordinal: usize,
        /// The child workflow name string literal (the spawn's first argument).
        name: String,
    },
    /// A control-flow node (a branch) that records no event of its own.
    ControlFlow {
        /// Zero-based position of this control-flow node among all control-flow
        /// nodes, in call order.
        ordinal: usize,
    },
}

/// A single node in the workflow graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphNode {
    /// Stable identity and call-order position of the node.
    pub id: NodeId,
    /// The workflow primitive the node represents.
    pub primitive: NodePrimitive,
    /// The per-node identity a consumer maps recorded events onto.
    pub correlation: CorrelationKey,
}

/// The relationship a directed edge expresses between two nodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EdgeKind {
    /// Sequential control flow: `from` precedes `to` in the workflow's order.
    Sequence,
    /// A branch arm: `from` is a `Branch` node, `to` is a node reached only on
    /// a particular case arm.
    Branch,
}

/// A directed edge between two nodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GraphEdge {
    /// Source node id.
    pub from: NodeId,
    /// Target node id.
    pub to: NodeId,
    /// The relationship the edge expresses.
    pub kind: EdgeKind,
}

/// A workflow's primitive structure as an ordered node/edge graph.
///
/// A projection of the typed source: extracted, never authored. The node and
/// edge vectors are in deterministic call order so two extractions of the same
/// package are byte-identical.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowGraph {
    /// The logical entry-module name the graph was extracted from.
    pub entry_module: String,
    /// Nodes in deterministic workflow call order.
    pub nodes: Vec<GraphNode>,
    /// Directed edges in deterministic order.
    pub edges: Vec<GraphEdge>,
}

impl WorkflowGraph {
    /// The logical entry-module name the graph was extracted from.
    #[must_use]
    pub fn entry_module(&self) -> &str {
        &self.entry_module
    }

    /// The graph's nodes in deterministic call order.
    #[must_use]
    pub fn nodes(&self) -> &[GraphNode] {
        &self.nodes
    }

    /// The graph's edges in deterministic order.
    #[must_use]
    pub fn edges(&self) -> &[GraphEdge] {
        &self.edges
    }

    /// Looks up a node by id.
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<&GraphNode> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// Whether this graph and `other` describe the same structure as unordered
    /// node and edge sets.
    ///
    /// This is the diff primitive the C23 test uses: it proves the extracted
    /// node/edge set matches a workflow's known structure regardless of the
    /// internal vector order, so a consumer can assert structural equality
    /// without depending on emission order.
    #[must_use]
    pub fn structurally_equals(&self, other: &Self) -> bool {
        if self.entry_module != other.entry_module {
            return false;
        }
        let self_nodes: BTreeSet<(NodeId, NodePrimitive, CorrelationKey)> = self
            .nodes
            .iter()
            .map(|node| (node.id, node.primitive, node.correlation.clone()))
            .collect();
        let other_nodes: BTreeSet<(NodeId, NodePrimitive, CorrelationKey)> = other
            .nodes
            .iter()
            .map(|node| (node.id, node.primitive, node.correlation.clone()))
            .collect();
        if self_nodes != other_nodes {
            return false;
        }
        let self_edges: BTreeSet<GraphEdge> = self.edges.iter().copied().collect();
        let other_edges: BTreeSet<GraphEdge> = other.edges.iter().copied().collect();
        self_edges == other_edges
    }
}
