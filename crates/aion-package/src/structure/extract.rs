//! Extract a workflow's primitive structure from its entry-module Gleam source.
//!
//! A Rust crate cannot type-check Gleam, so — following the codegen precedent
//! (a generator operates over a canonical derived representation, never by
//! type-checking Gleam) — extraction scans the entry-module source for the
//! fixed `aion/workflow` primitive vocabulary. This is sound only because that
//! vocabulary is small and known (ADR-014, P2). The supported subset is stated
//! and enforced below; anything outside it is reported, never silently
//! mis-parsed (no silent failures).
//!
//! ## Supported import/call subset
//!
//! - The entry module imports `aion/workflow`, optionally aliased
//!   (`import aion/workflow as <alias>`). Primitive calls are recognised as
//!   `<alias>.run`, `<alias>.all`, and so on. Absence of the import is a loud
//!   [`StructureError::NoWorkflowImport`], never an empty graph.
//! - A `run` node's activity name is read from the
//!   `<wrappers_alias>.<name>_activity(` call passed to `run`, where the
//!   wrappers module is imported as `import <pkg>_activity_wrappers as <alias>`
//!   (the form `aion generate` emits). The extracted name is validated against
//!   the manifest's declared activities.
//! - `start_timer`, `spawn`, and `spawn_and_wait` carry their first string
//!   literal argument as a name; it is read directly from source.
//!
//! ## Control flow
//!
//! Gleam `case` is pervasive pattern matching (every `workflow.run` result in a
//! saga is matched with `case`), not a workflow primitive. The extractor
//! therefore does *not* mint a [`NodePrimitive::Branch`] for every `case`: doing
//! so would make a compensating saga's graph wrong (its known structure is a
//! sequence of `run` nodes, C23). Branch nodes and branch edges remain in the
//! model for a consumer to introduce when a richer control-flow projection is
//! warranted; the source-text extractor's vocabulary is strictly the recorded
//! `aion/workflow` primitive calls.

use crate::Package;

use super::error::StructureError;
use super::model::{
    CorrelationKey, EdgeKind, GraphEdge, GraphNode, NodeId, NodePrimitive, WorkflowGraph,
};
use super::scan::{Token, tokenise};

const ACTIVITY_WRAPPER_SUFFIX: &str = "_activity";

/// Extracts the workflow graph model from a loaded package.
///
/// Reads the manifest entry module's verbatim Gleam source from
/// [`Package::source`], scans it for the `aion/workflow` primitive vocabulary,
/// and builds an ordered node/edge graph whose nodes carry the correlation key
/// a consumer overlays a run's recorded events onto (C21, C22, C23).
///
/// # Errors
///
/// Returns [`StructureError::MissingEntrySource`] when the entry module has no
/// source, [`StructureError::EntrySourceNotUtf8`] when its bytes are not UTF-8,
/// [`StructureError::NoWorkflowImport`] when it does not import `aion/workflow`,
/// and [`StructureError::UnknownActivity`] when a `run` node names an activity
/// the manifest does not declare.
pub fn extract_structure(package: &Package) -> Result<WorkflowGraph, StructureError> {
    let entry_module = package.manifest().entry_module.clone();
    let bytes =
        package
            .source()
            .get(&entry_module)
            .ok_or_else(|| StructureError::MissingEntrySource {
                module: entry_module.clone(),
            })?;
    let source = std::str::from_utf8(bytes).map_err(|_| StructureError::EntrySourceNotUtf8 {
        module: entry_module.clone(),
    })?;

    let tokens = tokenise(source);
    let workflow_alias =
        workflow_alias(&tokens).ok_or_else(|| StructureError::NoWorkflowImport {
            module: entry_module.clone(),
        })?;

    let declared: std::collections::BTreeSet<&str> = package
        .manifest()
        .activities
        .iter()
        .map(|activity| activity.activity_type.as_str())
        .collect();

    let mut builder = GraphBuilder::new(entry_module);
    let mut cursor = 0;
    while cursor < tokens.len() {
        if let Token::Qualified { left, right } = &tokens[cursor] {
            if *left == workflow_alias {
                if let Some(primitive) = recognise(right) {
                    let args = &tokens[cursor + 1..];
                    let correlation = builder.correlation_for(primitive, args, &declared)?;
                    builder.push(primitive, correlation);
                }
            }
        }
        cursor += 1;
    }

    Ok(builder.finish())
}

/// Recognises a `aion/workflow` member name as a node primitive, or `None` for
/// a member that does not introduce a graph node (for example `now`, `random`,
/// `id`, `define`, or the definition accessors).
fn recognise(member: &str) -> Option<NodePrimitive> {
    match member {
        "run" => Some(NodePrimitive::Run),
        "all" => Some(NodePrimitive::All),
        "race" => Some(NodePrimitive::Race),
        "map" => Some(NodePrimitive::Map),
        "spawn" => Some(NodePrimitive::Spawn),
        "spawn_and_wait" => Some(NodePrimitive::SpawnAndWait),
        "receive" => Some(NodePrimitive::Receive),
        "sleep" => Some(NodePrimitive::Sleep),
        "start_timer" => Some(NodePrimitive::StartTimer),
        "cancel_timer" => Some(NodePrimitive::CancelTimer),
        _ => None,
    }
}

/// Finds the alias the entry module imports `aion/workflow` under.
///
/// `import aion/workflow` yields `workflow` (the last path segment); an explicit
/// `import aion/workflow as <alias>` yields `<alias>`. Returns `None` when the
/// module does not import `aion/workflow` at all.
fn workflow_alias(tokens: &[Token]) -> Option<String> {
    let mut index = 0;
    while index < tokens.len() {
        if matches!(&tokens[index], Token::Ident(word) if word == "import")
            && import_path_is_workflow(tokens, index + 1)
        {
            // After the path, an optional `as <alias>` renames the import.
            if let Some(alias) = alias_after_import(tokens, index + 1) {
                return Some(alias);
            }
            return Some("workflow".to_owned());
        }
        index += 1;
    }
    None
}

/// Whether the import path tokens beginning at `start` spell `aion/workflow`.
///
/// The scanner renders `aion/workflow` as `Ident("aion")`, `Other('/')`,
/// `Ident("workflow")`; an aliased or selective import keeps the same path
/// prefix, so matching the two path identifiers around the slash is sufficient
/// and avoids matching `aion/workflow/timer` style submodule imports the entry
/// module does not orchestrate through.
fn import_path_is_workflow(tokens: &[Token], start: usize) -> bool {
    matches!(
        (tokens.get(start), tokens.get(start + 1), tokens.get(start + 2)),
        (
            Some(Token::Ident(first)),
            Some(Token::Other('/')),
            Some(Token::Ident(second)),
        ) if first == "aion"
            && second == "workflow"
            && !matches!(tokens.get(start + 3), Some(Token::Other('/')))
    )
}

/// Reads the alias of `import aion/workflow as <alias>`.
///
/// The path occupies three tokens (`aion`, `/`, `workflow`) from `path_start`;
/// an alias, if present, is the token after a literal `as` immediately
/// following the path. Anything else (a bare import, a selective
/// `import aion/workflow.{...}`, or the next statement) yields no alias here and
/// the caller falls back to the default `workflow`.
fn alias_after_import(tokens: &[Token], path_start: usize) -> Option<String> {
    let after_path = path_start + 3;
    if matches!(tokens.get(after_path), Some(Token::Ident(word)) if word == "as") {
        if let Some(Token::Ident(alias)) = tokens.get(after_path + 1) {
            return Some(alias.clone());
        }
    }
    None
}

/// Accumulates nodes and sequential edges in call order, assigning the
/// per-kind ordinals correlation keys carry.
struct GraphBuilder {
    entry_module: String,
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
    activity_ordinal: usize,
    child_ordinal: usize,
    control_ordinal: usize,
}

impl GraphBuilder {
    fn new(entry_module: String) -> Self {
        Self {
            entry_module,
            nodes: Vec::new(),
            edges: Vec::new(),
            activity_ordinal: 0,
            child_ordinal: 0,
            control_ordinal: 0,
        }
    }

    /// Builds the correlation key for a primitive from the tokens following its
    /// call, advancing the relevant ordinal counter.
    fn correlation_for(
        &mut self,
        primitive: NodePrimitive,
        args: &[Token],
        declared: &std::collections::BTreeSet<&str>,
    ) -> Result<CorrelationKey, StructureError> {
        match primitive {
            NodePrimitive::Run => {
                let activity =
                    run_activity_name(args).ok_or_else(|| StructureError::UnknownActivity {
                        activity: String::new(),
                    })?;
                if !declared.contains(activity.as_str()) {
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
            | NodePrimitive::Branch => {
                let ordinal = self.control_ordinal;
                self.control_ordinal += 1;
                Ok(CorrelationKey::ControlFlow { ordinal })
            }
        }
    }

    /// Appends a node and a sequential edge from the previous node.
    fn push(&mut self, primitive: NodePrimitive, correlation: CorrelationKey) {
        let id = NodeId(self.nodes.len());
        if let Some(previous) = self.nodes.last() {
            self.edges.push(GraphEdge {
                from: previous.id,
                to: id,
                kind: EdgeKind::Sequence,
            });
        }
        self.nodes.push(GraphNode {
            id,
            primitive,
            correlation,
        });
    }

    fn finish(self) -> WorkflowGraph {
        WorkflowGraph {
            entry_module: self.entry_module,
            nodes: self.nodes,
            edges: self.edges,
        }
    }
}

/// Reads the activity name from a `run(<wrappers>.<name>_activity(...))` call.
///
/// The first token after `run`'s open paren must be a qualified call whose
/// member ends with `_activity`; the activity name is that member with the
/// suffix stripped. Returns `None` for any other shape, so a `run` the
/// extractor cannot resolve is a loud error rather than a blank node.
fn run_activity_name(args: &[Token]) -> Option<String> {
    let mut index = 0;
    // Skip a single leading open paren of the `run(` call.
    if matches!(args.first(), Some(Token::OpenParen)) {
        index = 1;
    }
    if let Some(Token::Qualified { left: _, right }) = args.get(index) {
        if let Some(name) = right.strip_suffix(ACTIVITY_WRAPPER_SUFFIX) {
            if !name.is_empty() {
                return Some(name.to_owned());
            }
        }
    }
    None
}

/// Reads the first string literal appearing as a call argument, scanning to the
/// matching close paren of the call. Returns `None` if no literal precedes it.
fn first_string_literal(args: &[Token]) -> Option<String> {
    let mut depth = 0_i32;
    for token in args {
        match token {
            Token::OpenParen => depth += 1,
            Token::CloseParen => {
                depth -= 1;
                if depth <= 0 {
                    return None;
                }
            }
            Token::StringLiteral(literal) if depth >= 1 => return Some(literal.clone()),
            _ => {}
        }
    }
    None
}

/// Reads the first non-paren argument as a reference token (an identifier or
/// qualified reference), for primitives whose first argument is a value rather
/// than a literal (`receive`, `cancel_timer`).
fn first_argument_reference(args: &[Token]) -> Option<String> {
    let mut depth = 0_i32;
    for token in args {
        match token {
            Token::OpenParen => depth += 1,
            Token::CloseParen => {
                depth -= 1;
                if depth <= 0 {
                    return None;
                }
            }
            Token::Ident(word) if depth >= 1 => return Some(word.clone()),
            Token::Qualified { left, right } if depth >= 1 => {
                return Some(format!("{left}.{right}"));
            }
            _ => {}
        }
    }
    None
}
