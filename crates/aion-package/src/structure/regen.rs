//! Bounded structural round-trip: regenerate type-checking Gleam from a delta.
//!
//! The graph model is a projection, never the authoritative artifact (CN6,
//! ADR-014). A consumer may apply one of a deliberately narrow set of
//! structural deltas and ask for Gleam that still type-checks; the typed source
//! remains the source of truth, and the returned String is for the consumer to
//! review and write, not something this layer writes back into the package.
//!
//! The vocabulary is intentionally tiny — appending or removing a sequential
//! `run` node — because unbounded diagram-to-code synthesis is explicitly out
//! of scope (the boundary forbids it). A delta outside the set, or one naming an
//! activity the package does not declare, is refused rather than emitting code.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::Package;

use super::error::StructureError;
use super::ident::{is_reserved_word, is_snake_identifier};
use super::model::{CorrelationKey, NodeId, NodePrimitive, WorkflowGraph};

/// A bounded structural edit to a workflow graph.
///
/// The set is deliberately narrow (CN6): it is enough to prove the round-trip
/// regenerates type-checking Gleam without becoming an unbounded second
/// authoring surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StructuralDelta {
    /// Append a sequential `run` node for `activity` immediately after `after`.
    ///
    /// `activity` must be declared by the package manifest; an unknown activity
    /// is refused (the canvas never invents an activity the source lacks).
    AppendRun {
        /// The activity the new `run` node dispatches.
        activity: String,
        /// The node the new node is sequenced after.
        after: NodeId,
    },
    /// Remove the node with this id (and re-sequence around it).
    RemoveNode {
        /// The node to remove.
        id: NodeId,
    },
}

/// Applies a bounded structural delta to `graph` and regenerates a complete,
/// self-contained Gleam workflow module that still type-checks against
/// `aion_flow` (C24).
///
/// The regenerated module represents the graph's `run`-node chain over the
/// package's declared activities. It is a projection: it does not mutate
/// `package` or `graph`, and the typed source remains authoritative.
///
/// # Errors
///
/// Returns [`StructureError::UnboundedDelta`] when the delta is outside the
/// bounded set (for example, the only node primitives the round-trip emits are
/// `run` chains, so a graph carrying a non-`run` node cannot be regenerated),
/// [`StructureError::DeltaTargetMissing`] when the delta targets an absent node,
/// [`StructureError::UnknownActivity`] when an appended activity is not declared
/// by the manifest, and [`StructureError::RegenInvalidName`] when a name would
/// not be a valid Gleam identifier.
pub fn regenerate_gleam(
    package: &Package,
    graph: &WorkflowGraph,
    delta: &StructuralDelta,
) -> Result<String, StructureError> {
    let declared: BTreeSet<&str> = package
        .manifest()
        .activities
        .iter()
        .map(|activity| activity.activity_type.as_str())
        .collect();

    let mut activities = run_chain(graph)?;
    apply(&mut activities, graph, delta, &declared)?;
    for activity in &activities {
        validate_name(activity)?;
    }
    Ok(emit_module(&activities))
}

/// Extracts the ordered list of activity names from a graph whose nodes are all
/// sequential `run` nodes. Any non-`run` node makes the graph outside the
/// bounded round-trip's emittable shape.
fn run_chain(graph: &WorkflowGraph) -> Result<Vec<String>, StructureError> {
    let mut activities = Vec::with_capacity(graph.nodes.len());
    for node in &graph.nodes {
        match (&node.primitive, &node.correlation) {
            (NodePrimitive::Run, CorrelationKey::ActivitySequence { activity, .. }) => {
                activities.push(activity.clone());
            }
            _ => {
                return Err(StructureError::UnboundedDelta {
                    reason: format!(
                        "node {} is a {:?}, but the bounded round-trip regenerates `run` chains \
                         only; regenerating arbitrary control flow is unbounded synthesis",
                        node.id.0, node.primitive
                    ),
                });
            }
        }
    }
    Ok(activities)
}

fn apply(
    activities: &mut Vec<String>,
    graph: &WorkflowGraph,
    delta: &StructuralDelta,
    declared: &BTreeSet<&str>,
) -> Result<(), StructureError> {
    match delta {
        StructuralDelta::AppendRun { activity, after } => {
            if !declared.contains(activity.as_str()) {
                return Err(StructureError::UnknownActivity {
                    activity: activity.clone(),
                });
            }
            let position = node_position(graph, *after)?;
            activities.insert(position + 1, activity.clone());
            Ok(())
        }
        StructuralDelta::RemoveNode { id } => {
            let position = node_position(graph, *id)?;
            activities.remove(position);
            Ok(())
        }
    }
}

/// Resolves a node id to its index within the `run` chain.
fn node_position(graph: &WorkflowGraph, id: NodeId) -> Result<usize, StructureError> {
    graph
        .nodes
        .iter()
        .position(|node| node.id == id)
        .ok_or(StructureError::DeltaTargetMissing { id: id.0 })
}

fn validate_name(activity: &str) -> Result<(), StructureError> {
    if !is_snake_identifier(activity) {
        return Err(StructureError::RegenInvalidName {
            name: activity.to_owned(),
            reason: "must be a snake_case identifier (a lowercase letter followed by lowercase \
                     letters, digits, or underscores)"
                .to_owned(),
        });
    }
    if is_reserved_word(activity) {
        return Err(StructureError::RegenInvalidName {
            name: activity.to_owned(),
            reason: "is a Gleam reserved word and cannot name a generated function".to_owned(),
        });
    }
    Ok(())
}

/// Emits a complete, self-contained Gleam workflow module for the `run` chain.
///
/// The module type-checks against `aion_flow` alone: each activity is a
/// `String -> String` activity built with `activity.new`, and `execute` chains
/// them with `workflow.run`, threading each result forward. There are no
/// invented retry policies, timeouts, or other defaults (ADR-001); an activity
/// built with `activity.new` carries none, which is the SDK's documented
/// no-config form.
fn emit_module(activities: &[String]) -> String {
    let mut out = String::new();
    out.push_str(
        "//// Regenerated by aion structure round-trip — a projection of the typed source.\n\
         //// The typed module remains the single source of truth (ADR-014); this is for review.\n\n\
         import aion/activity\n\
         import aion/codec\n\
         import aion/error\n\
         import aion/workflow\n\n\
         fn string_codec() -> codec.Codec(String) {\n\
         \u{20}\u{20}codec.Codec(encode: fn(value) { value }, decode: fn(input) { Ok(input) })\n\
         }\n\n",
    );

    let mut emitted: BTreeSet<&str> = BTreeSet::new();
    for activity in activities {
        if !emitted.insert(activity.as_str()) {
            // The same activity may appear at several positions in the chain;
            // its wrapper function is defined once. Gleam module names must be
            // unique, so a repeated dispatch reuses the one definition.
            continue;
        }
        // Writing to a `String` is infallible; the discarded `Result` follows
        // the codegen module's established `std::fmt::Write` emission pattern.
        let _ = writeln!(
            out,
            "fn {activity}_activity(\n\
             \u{20}\u{20}input: String,\n\
             ) -> activity.Activity(String, String) {{\n\
             \u{20}\u{20}activity.new(\n\
             \u{20}\u{20}\u{20}\u{20}\"{activity}\",\n\
             \u{20}\u{20}\u{20}\u{20}input,\n\
             \u{20}\u{20}\u{20}\u{20}string_codec(),\n\
             \u{20}\u{20}\u{20}\u{20}string_codec(),\n\
             \u{20}\u{20}\u{20}\u{20}fn(value) {{ Ok(value) }},\n\
             \u{20}\u{20})\n\
             }}\n"
        );
    }

    out.push_str("pub fn execute(input: String) -> Result(String, error.ActivityError) {\n");
    if activities.is_empty() {
        out.push_str("  Ok(input)\n}\n");
        return out;
    }
    emit_chain(&mut out, activities, 0);
    out
}

/// Emits the nested `case workflow.run(...)` chain that threads each activity's
/// output into the next, returning the final output or the first error.
///
/// Writing to a `String` is infallible; discarded `Result`s follow the codegen
/// module's established `std::fmt::Write` emission pattern.
fn emit_chain(out: &mut String, activities: &[String], index: usize) {
    let indent = "  ".repeat(index + 1);
    let activity = &activities[index];
    let value = if index == 0 {
        "input".to_owned()
    } else {
        format!("value_{}", index - 1)
    };
    let _ = writeln!(
        out,
        "{indent}case workflow.run({activity}_activity({value})) {{"
    );
    let inner = "  ".repeat(index + 2);
    if index + 1 == activities.len() {
        let _ = writeln!(out, "{inner}Ok(output) -> Ok(output)");
    } else {
        let _ = writeln!(out, "{inner}Ok(value_{index}) -> {{");
        emit_chain(out, activities, index + 1);
        let _ = writeln!(out, "{inner}}}");
    }
    let _ = writeln!(out, "{inner}Error(activity_error) -> Error(activity_error)");
    let _ = writeln!(out, "{indent}}}");
    if index == 0 {
        out.push_str("}\n");
    }
}
