//! Node and edge accumulation for the control-flow walker.
//!
//! These methods own the graph's growth: minting primitive, fan-out member,
//! branch, and opaque nodes; assigning the per-kind correlation ordinals; and
//! the checkpoint/restore the walker uses to prune a primitive-free region
//! without leaving a node, edge, or skipped ordinal behind. They are split from
//! the walker (`super`) only to keep each file under the size budget; they
//! operate on the same [`ControlFlowExtractor`] state.

use super::super::error::StructureError;
use super::super::model::{CorrelationKey, EdgeKind, GraphEdge, GraphNode, NodeId, NodePrimitive};
use super::super::reader::{
    fan_out_members, first_argument_reference, first_string_literal, run_activity_name, snippet,
};
use super::super::scan::Token;
use super::{Checkpoint, ControlFlowExtractor, Frontier, OPAQUE_SNIPPET_TOKENS};

impl ControlFlowExtractor<'_> {
    /// Emits a primitive node from the tokens at `index`, fanning out concurrency
    /// members for `all` / `race` / `map`.
    pub(super) fn emit_primitive(
        &mut self,
        primitive: NodePrimitive,
        index: usize,
    ) -> Result<NodeId, StructureError> {
        let args = &self.tokens[index + 1..];
        let correlation = self.correlation_for(primitive, args)?;
        let node = self.push_node(primitive, correlation);
        if matches!(
            primitive,
            NodePrimitive::All | NodePrimitive::Race | NodePrimitive::Map
        ) {
            self.emit_fan_out_members(node, args)?;
        }
        Ok(node)
    }

    /// Emits a `Run` member node per fan-out activity under a concurrency node,
    /// each with its own `ActivitySequence` key and a `FanOut` edge from the
    /// concurrency node — so a consumer can overlay each member's recorded
    /// `ActivityCompleted` independently (the fan-out review item). Each member
    /// is validated against the manifest's declared activities.
    fn emit_fan_out_members(
        &mut self,
        concurrency: NodeId,
        args: &[Token],
    ) -> Result<(), StructureError> {
        for activity in fan_out_members(args) {
            if !self.declared.contains(activity.as_str()) {
                return Err(StructureError::UnknownActivity { activity });
            }
            let ordinal = self.activity_ordinal;
            self.activity_ordinal += 1;
            let member = self.push_node(
                NodePrimitive::Run,
                CorrelationKey::ActivitySequence { ordinal, activity },
            );
            self.edges.push(GraphEdge {
                from: concurrency,
                to: member,
                kind: EdgeKind::FanOut,
            });
        }
        Ok(())
    }

    /// Builds the correlation key for a primitive, advancing the relevant
    /// per-kind ordinal counter.
    fn correlation_for(
        &mut self,
        primitive: NodePrimitive,
        args: &[Token],
    ) -> Result<CorrelationKey, StructureError> {
        match primitive {
            NodePrimitive::Run => {
                let activity =
                    run_activity_name(args).ok_or_else(|| StructureError::UnknownActivity {
                        activity: String::new(),
                    })?;
                if !self.declared.contains(activity.as_str()) {
                    return Err(StructureError::UnknownActivity { activity });
                }
                let ordinal = self.activity_ordinal;
                self.activity_ordinal += 1;
                Ok(CorrelationKey::ActivitySequence { ordinal, activity })
            }
            NodePrimitive::Spawn | NodePrimitive::SpawnAndWait => {
                let name = first_string_literal(args).unwrap_or_default();
                let ordinal = self.child_ordinal;
                self.child_ordinal += 1;
                Ok(CorrelationKey::Child { ordinal, name })
            }
            NodePrimitive::Receive => Ok(CorrelationKey::Signal {
                reference: first_argument_reference(args).unwrap_or_default(),
            }),
            NodePrimitive::StartTimer => Ok(CorrelationKey::Timer {
                id: first_string_literal(args).unwrap_or_default(),
            }),
            NodePrimitive::CancelTimer => Ok(CorrelationKey::Timer {
                id: first_argument_reference(args).unwrap_or_default(),
            }),
            NodePrimitive::All
            | NodePrimitive::Race
            | NodePrimitive::Map
            | NodePrimitive::Sleep
            | NodePrimitive::Branch
            | NodePrimitive::Opaque => {
                let ordinal = self.control_ordinal;
                self.control_ordinal += 1;
                Ok(CorrelationKey::ControlFlow { ordinal })
            }
        }
    }

    /// Mints a branch node, advancing the control-flow ordinal.
    pub(super) fn emit_branch(&mut self) -> NodeId {
        let ordinal = self.control_ordinal;
        self.control_ordinal += 1;
        self.push_node(
            NodePrimitive::Branch,
            CorrelationKey::ControlFlow { ordinal },
        )
    }

    /// Mints an opaque node carrying a short snippet of the unmodellable shape.
    pub(super) fn emit_opaque(&mut self, start: usize, end: usize) -> NodeId {
        let upper = end.min(start + OPAQUE_SNIPPET_TOKENS);
        let text = snippet(&self.tokens[start..upper]);
        let ordinal = self.opaque_ordinal;
        self.opaque_ordinal += 1;
        self.push_node(
            NodePrimitive::Opaque,
            CorrelationKey::Opaque {
                ordinal,
                snippet: text,
            },
        )
    }

    fn push_node(&mut self, primitive: NodePrimitive, correlation: CorrelationKey) -> NodeId {
        let id = NodeId(self.nodes.len());
        self.nodes.push(GraphNode {
            id,
            primitive,
            correlation,
        });
        id
    }

    /// Sequences `to` after each node in `frontier`, skipping self-edges.
    pub(super) fn sequence(&mut self, frontier: &Frontier, to: NodeId) {
        for from in frontier {
            if *from != to {
                self.edges.push(GraphEdge {
                    from: *from,
                    to,
                    kind: EdgeKind::Sequence,
                });
            }
        }
    }

    /// Snapshots the accumulation point before a region that may be pruned.
    pub(super) fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            nodes: self.nodes.len(),
            edges: self.edges.len(),
            activity_ordinal: self.activity_ordinal,
            child_ordinal: self.child_ordinal,
            control_ordinal: self.control_ordinal,
            opaque_ordinal: self.opaque_ordinal,
        }
    }

    /// Restores a checkpoint, dropping every node, edge, and ordinal advance a
    /// pruned (primitive-free) region produced.
    pub(super) fn restore(&mut self, checkpoint: Checkpoint) {
        self.nodes.truncate(checkpoint.nodes);
        self.edges.truncate(checkpoint.edges);
        self.activity_ordinal = checkpoint.activity_ordinal;
        self.child_ordinal = checkpoint.child_ordinal;
        self.control_ordinal = checkpoint.control_ordinal;
        self.opaque_ordinal = checkpoint.opaque_ordinal;
    }
}
