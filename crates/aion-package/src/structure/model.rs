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
/// The id is the node's deterministic position in the order the extractor
/// discovers nodes while walking the workflow's control flow depth-first from
/// the entry function (success arms before error arms). It is a stable static
/// position, not a claim about runtime execution order: under branching, which
/// nodes a run actually executes is data-dependent. The id is opaque to
/// consumers beyond identity and ordering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub usize);

/// The workflow primitive a node represents.
///
/// This is the fixed, known vocabulary of `aion/workflow` (the only surface the
/// extractor understands), plus `Branch` for a `case` over a primitive result
/// and `Opaque` for control flow the extractor recognises as present but cannot
/// faithfully model. Adding a primitive to the SDK requires adding it here; an
/// unrecognised call is never silently dropped, and control flow the walker
/// cannot resolve is surfaced as an `Opaque` node, never flattened into a false
/// sequential edge.
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
    /// A `case` expression branching on a workflow-primitive result: its arms
    /// fan out to the real success/error subgraphs via labelled branch edges.
    Branch,
    /// Control flow the extractor detects but cannot faithfully model (a `case`
    /// whose arms it cannot bound, a recursive helper call, an indirect
    /// dispatch). It is surfaced as an explicit node carrying the offending
    /// source snippet — the honest alternative to silently flattening an
    /// unmodellable shape into a false sequence (the loud-on-unmodellable rule).
    Opaque,
}

/// The per-node identity a consumer maps recorded events onto.
///
/// The event-bearing kinds mirror how AD/AT sequences recorded events:
/// activities by their static source position plus name, signals by name,
/// timers by id, and children by spawn ordinal plus name. `ControlFlow` carries
/// no recorded event of its own; it positions a structural node (a branch) in
/// the discovery order. `Opaque` carries the source snippet of control flow the
/// extractor could not model, so a consumer can surface it for review.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CorrelationKey {
    /// An activity dispatch: its static source-order position and activity name.
    ActivitySequence {
        /// Zero-based static source-order position of this activity among all
        /// activity dispatches the extractor discovers, walking control flow
        /// depth-first from the entry function (success arms before error
        /// arms). It is a stable structural index, NOT a claim about the order
        /// replay re-applies recorded events: under branching, which activities
        /// a given run executes — and in what order — is data-dependent. A
        /// consumer aligns a recorded `ActivityCompleted` onto a node by the
        /// activity name and the branch the run actually took, not by assuming
        /// this ordinal equals the recorded sequence position.
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
        /// nodes, in discovery order.
        ordinal: usize,
    },
    /// An unmodellable control-flow node: control flow the extractor detected
    /// but could not faithfully classify, carrying the offending source snippet
    /// so a consumer can render it for review. It records no event of its own.
    Opaque {
        /// Zero-based position of this opaque node among all opaque nodes, in
        /// discovery order.
        ordinal: usize,
        /// The source snippet the extractor could not model.
        snippet: String,
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

/// Which arm of a `Branch` node a branch edge belongs to.
///
/// A `case` over a workflow primitive's `Result` has an `Ok(..)` arm (the
/// success continuation) and an `Error(..)` arm (the compensation / failure
/// continuation). A `case` over another subject the walker recurses through
/// (a decode, a guard) labels each arm by its leading constructor, or
/// [`ArmLabel::Wildcard`] for a `_` catch-all.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArmLabel {
    /// The `Ok(..)` arm — the success continuation.
    Ok,
    /// The `Error(..)` arm — the failure / compensation continuation.
    Error,
    /// A `_` catch-all arm.
    Wildcard,
    /// Any other arm, labelled by its leading pattern constructor.
    Pattern(String),
}

/// The relationship a directed edge expresses between two nodes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EdgeKind {
    /// Sequential control flow: `from` precedes `to` in the workflow's order.
    Sequence,
    /// A branch arm: `from` is a `Branch` node, `to` is the first node reached
    /// on the `arm` of that `case`. The label distinguishes the success arm
    /// from the compensation arm so the extracted edge set matches the
    /// workflow's actual control flow rather than a linear flatten.
    Branch {
        /// Which arm of the branch this edge follows.
        arm: ArmLabel,
    },
    /// A concurrent fan-out member: `from` is an `All` / `Race` / `Map` node,
    /// `to` is one member activity dispatched concurrently under it. The member
    /// nodes carry their own `ActivitySequence` keys so a consumer can overlay
    /// each member's recorded `ActivityCompleted` independently.
    FanOut,
}

/// A directed edge between two nodes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
        let self_edges: BTreeSet<GraphEdge> = self.edges.iter().cloned().collect();
        let other_edges: BTreeSet<GraphEdge> = other.edges.iter().cloned().collect();
        self_edges == other_edges
    }
}
