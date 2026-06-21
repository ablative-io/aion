//! Faithful control-flow extraction over the entry-module token stream.
//!
//! The flat scanner this module replaces walked the whole entry module top to
//! bottom and chained every recognised `aion/workflow` primitive into one linear
//! `Sequence` edge list, ignoring `case` entirely. For a compensating saga that
//! is wrong: the compensations (`release` / `refund`) are reached only on the
//! `Error` arm of the `reserve` / `charge` / `ship` `case` expressions, and live
//! in helper functions the flat scanner never followed.
//!
//! This walker is control-flow faithful. Starting at the manifest entry
//! function, it recurses the function's body in source order, threading a
//! *frontier* (the nodes the next statement is sequenced after). A `case` over a
//! workflow-primitive result mints the primitive node and a
//! [`NodePrimitive::Branch`] whose arm edges lead into the real success and
//! compensation subgraphs; a `case` over a local-function call follows that
//! function inline before branching on its outcome; a `case` it cannot bound or
//! resolve becomes an explicit [`NodePrimitive::Opaque`] node carrying the source
//! snippet — never a false sequential edge (the loud-on-unmodellable rule).
//!
//! It is not a Gleam type-checker (a Rust crate cannot type-check Gleam); it
//! operates over the derived token stream, the codegen module's established
//! approach. Correctness here is *never assert a false edge*, not *parse all
//! Gleam*: any shape outside the recognised surface is surfaced loudly.

mod emit;

use std::collections::{BTreeMap, BTreeSet};

use super::arms::{arm_label, split_arms};
use super::error::StructureError;
use super::model::{EdgeKind, GraphEdge, GraphNode, NodeId, NodePrimitive};
use super::reader::{end_of_call, find_open_brace, leading_local_call, match_brace};
use super::scan::Token;

/// The number of tokens of an unmodellable shape captured into an `Opaque`
/// node's snippet, enough for a consumer to locate it without copying a whole
/// function body.
const OPAQUE_SNIPPET_TOKENS: usize = 12;

/// The nodes the next statement should be sequenced after.
type Frontier = Vec<NodeId>;

/// A region of walked source: the first node it produced (for branch-edge
/// attachment), the frontier left after it, and how many *workflow primitive*
/// nodes (run / all / race / map / spawn / receive / sleep / timers) it
/// contributed. A region with zero primitives carries no durable workflow
/// structure and is pruned, so pure computation (error formatting, decode
/// plumbing with no primitive) never appears as a false branch or node.
#[derive(Clone, Default)]
struct Region {
    head: Option<NodeId>,
    tail: Frontier,
    primitives: usize,
}

impl Region {
    fn empty(frontier: Frontier) -> Self {
        Self {
            head: None,
            tail: frontier,
            primitives: 0,
        }
    }
}

/// A function body as the half-open token range strictly inside its `{ }`.
#[derive(Clone, Copy)]
struct FnBody {
    start: usize,
    end: usize,
}

/// The classification of a `case` scrutinee.
enum Subject {
    /// A workflow primitive (or a local call that yields one): a durable branch,
    /// carrying the subject node region to hang the branch off.
    Primitive(Region),
    /// Pure data (a decode, an error-enum format, a guard) the workflow branches
    /// on; transparent unless two or more arms carry a primitive.
    Data,
}

/// A snapshot of the extractor's accumulation point, taken before walking a
/// region that may be pruned. Restoring it removes every node, edge, and
/// ordinal the pruned region advanced — so a primitive-free region leaves no
/// trace (no false node, no false edge, no skipped ordinal).
#[derive(Clone, Copy)]
struct Checkpoint {
    nodes: usize,
    edges: usize,
    activity_ordinal: usize,
    child_ordinal: usize,
    control_ordinal: usize,
    opaque_ordinal: usize,
}

/// The extracted graph parts the caller assembles into a `WorkflowGraph`.
pub(super) struct ExtractedGraph {
    pub(super) nodes: Vec<GraphNode>,
    pub(super) edges: Vec<GraphEdge>,
}

/// Drives the recursive control-flow walk and accumulates the graph.
pub(super) struct ControlFlowExtractor<'a> {
    entry_module: String,
    tokens: &'a [Token],
    functions: BTreeMap<String, FnBody>,
    declared: &'a BTreeSet<&'a str>,
    workflow_alias: String,
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
    activity_ordinal: usize,
    child_ordinal: usize,
    control_ordinal: usize,
    opaque_ordinal: usize,
}

impl<'a> ControlFlowExtractor<'a> {
    pub(super) fn new(
        entry_module: String,
        tokens: &'a [Token],
        workflow_alias: String,
        declared: &'a BTreeSet<&'a str>,
    ) -> Self {
        let functions = map_functions(tokens);
        Self {
            entry_module,
            tokens,
            functions,
            declared,
            workflow_alias,
            nodes: Vec::new(),
            edges: Vec::new(),
            activity_ordinal: 0,
            child_ordinal: 0,
            control_ordinal: 0,
            opaque_ordinal: 0,
        }
    }

    /// Walks the workflow from `entry_function`, returning the faithful graph.
    ///
    /// # Errors
    ///
    /// Returns [`StructureError::EntryFunctionNotFound`] when the entry function
    /// is not defined, and [`StructureError::UnknownActivity`] when a `run` node
    /// names an activity the manifest does not declare.
    pub(super) fn extract(
        mut self,
        entry_function: &str,
    ) -> Result<ExtractedGraph, StructureError> {
        let body = self.functions.get(entry_function).copied().ok_or_else(|| {
            StructureError::EntryFunctionNotFound {
                module: self.entry_module.clone(),
                function: entry_function.to_owned(),
            }
        })?;
        let mut stack: Vec<String> = vec![entry_function.to_owned()];
        self.walk(body, Vec::new(), &mut stack)?;
        Ok(ExtractedGraph {
            nodes: self.nodes,
            edges: self.edges,
        })
    }

    /// Walks `[body.start, body.end)` in source order, threading `frontier`, and
    /// returns the region (head node, post-body frontier, primitive count).
    fn walk(
        &mut self,
        body: FnBody,
        frontier: Frontier,
        stack: &mut Vec<String>,
    ) -> Result<Region, StructureError> {
        let mut region = Region::empty(frontier);
        let mut index = body.start;
        while index < body.end {
            match &self.tokens[index] {
                Token::Ident(word) if word == "case" => {
                    let (case_region, after) = self.walk_case(index, &region.tail, body, stack)?;
                    region.head = region.head.or(case_region.head);
                    if !case_region.tail.is_empty() || case_region.primitives > 0 {
                        region.tail = case_region.tail;
                    }
                    region.primitives += case_region.primitives;
                    index = after;
                }
                Token::Qualified { left, right } if *left == self.workflow_alias => {
                    if let Some(primitive) = recognise(right) {
                        let node = self.emit_primitive(primitive, index)?;
                        self.sequence(&region.tail, node);
                        region.head = region.head.or(Some(node));
                        region.tail = vec![node];
                        region.primitives += 1;
                        index = end_of_call(self.tokens, index, body.end);
                        continue;
                    }
                    index += 1;
                }
                Token::Ident(name) if self.is_local_call(name, index) => {
                    let name = name.clone();
                    let call = self.follow_named(&name, &region.tail, stack)?;
                    if call.primitives > 0 {
                        region.head = region.head.or(call.head);
                        region.tail = call.tail;
                        region.primitives += call.primitives;
                    }
                    index = end_of_call(self.tokens, index, body.end);
                }
                _ => index += 1,
            }
        }
        Ok(region)
    }

    /// Walks a body that may be pruned: takes a checkpoint, walks, and if the
    /// body contributed no workflow primitive, restores the checkpoint so the
    /// pruned region leaves no node, edge, or ordinal behind.
    fn walk_pruned(
        &mut self,
        body: FnBody,
        frontier: Frontier,
        stack: &mut Vec<String>,
    ) -> Result<Region, StructureError> {
        let checkpoint = self.checkpoint();
        let region = self.walk(body, frontier.clone(), stack)?;
        if region.primitives == 0 {
            self.restore(checkpoint);
            return Ok(Region::empty(frontier));
        }
        Ok(region)
    }

    /// Handles a `case` beginning at `case_index`. Returns its region and the
    /// index just past the `case` body.
    ///
    /// A `case` over a workflow-primitive result (or a local call that yields a
    /// primitive) is a durable branch: it mints a [`NodePrimitive::Branch`] with
    /// labelled arm edges into the real success / compensation subgraphs. A
    /// `case` over pure data (a decode, an error-enum format, a guard) is
    /// transparent: its arms are walked and pruned, and only if two or more arms
    /// carry a primitive — a genuine data-driven fork — is a branch minted.
    fn walk_case(
        &mut self,
        case_index: usize,
        frontier: &Frontier,
        body: FnBody,
        stack: &mut Vec<String>,
    ) -> Result<(Region, usize), StructureError> {
        let Some(brace) = find_open_brace(self.tokens, case_index + 1, body.end) else {
            return Ok((self.opaque_region(case_index, body.end, frontier), body.end));
        };
        let Some(close) = match_brace(self.tokens, brace, body.end) else {
            return Ok((self.opaque_region(case_index, body.end, frontier), body.end));
        };
        let scrutinee = (case_index + 1, brace);
        let arms = split_arms(self.tokens, brace + 1, close);

        let region = match self.classify_subject(scrutinee, frontier, stack)? {
            Subject::Primitive(subject) => self.durable_branch(&subject, &arms, stack)?,
            Subject::Data => self.data_case(&arms, frontier, stack)?,
        };
        Ok((region, close + 1))
    }

    /// Builds a durable branch: a [`NodePrimitive::Branch`] sequenced after the
    /// subject's tail, with each non-empty arm attached by a labelled branch
    /// edge into its subgraph.
    fn durable_branch(
        &mut self,
        subject: &Region,
        arms: &[super::arms::Arm],
        stack: &mut Vec<String>,
    ) -> Result<Region, StructureError> {
        let branch = self.emit_branch();
        self.sequence(&subject.tail, branch);
        let head = subject.head.or(Some(branch));

        let mut merged: Frontier = Vec::new();
        let mut has_terminal_arm = false;
        for arm in arms {
            let label = arm_label(self.tokens, arm);
            let arm_body = FnBody {
                start: arm.body_start,
                end: arm.body_end,
            };
            let arm_region = self.walk_pruned(arm_body, Vec::new(), stack)?;
            if let Some(arm_head) = arm_region.head {
                self.edges.push(GraphEdge {
                    from: branch,
                    to: arm_head,
                    kind: EdgeKind::Branch { arm: label },
                });
                merged.extend(arm_region.tail);
            } else {
                // A terminal arm (e.g. `Ok(output) -> Ok(output)`) completes via
                // the branch itself: the branch node is an exit for that path.
                has_terminal_arm = true;
            }
        }
        if has_terminal_arm || merged.is_empty() {
            merged.push(branch);
        }
        Ok(Region {
            head,
            tail: merged,
            primitives: subject.primitives.max(1),
        })
    }

    /// Handles a `case` over pure data: transparent unless two or more arms
    /// carry a primitive (a real data-driven fork).
    fn data_case(
        &mut self,
        arms: &[super::arms::Arm],
        frontier: &Frontier,
        stack: &mut Vec<String>,
    ) -> Result<Region, StructureError> {
        // Walk each arm rootless and pruned; the arm head, if any, hangs off the
        // branch (multi-arm) or the incoming frontier (single-arm).
        let mut bearing: Vec<(super::model::ArmLabel, Region)> = Vec::new();
        for arm in arms {
            let label = arm_label(self.tokens, arm);
            let arm_body = FnBody {
                start: arm.body_start,
                end: arm.body_end,
            };
            let arm_region = self.walk_pruned(arm_body, Vec::new(), stack)?;
            if arm_region.head.is_some() {
                bearing.push((label, arm_region));
            }
        }
        match bearing.len() {
            0 => Ok(Region::empty(frontier.clone())),
            1 => {
                // Exactly one arm carries primitives: thread it through with no
                // false branch node. Its head is sequenced from the frontier.
                let (_, arm) = bearing.remove(0);
                if let Some(head) = arm.head {
                    self.sequence(frontier, head);
                }
                Ok(Region {
                    head: arm.head,
                    tail: arm.tail,
                    primitives: arm.primitives,
                })
            }
            _ => {
                // A genuine data-driven fork: mint a branch.
                let branch = self.emit_branch();
                self.sequence(frontier, branch);
                let mut merged: Frontier = Vec::new();
                let mut primitives = 0;
                for (label, arm) in bearing {
                    if let Some(arm_head) = arm.head {
                        self.edges.push(GraphEdge {
                            from: branch,
                            to: arm_head,
                            kind: EdgeKind::Branch { arm: label },
                        });
                    }
                    merged.extend(arm.tail);
                    primitives += arm.primitives;
                }
                Ok(Region {
                    head: Some(branch),
                    tail: merged,
                    primitives: primitives.max(1),
                })
            }
        }
    }

    /// Classifies a `case` scrutinee and, for a primitive or primitive-bearing
    /// local call, emits the subject node(s) sequenced from `frontier`.
    fn classify_subject(
        &mut self,
        scrutinee: (usize, usize),
        frontier: &Frontier,
        stack: &mut Vec<String>,
    ) -> Result<Subject, StructureError> {
        let (start, end) = scrutinee;
        // 1. `case workflow.<primitive>(...)`.
        if let Some(prim_index) = self.scrutinee_primitive(start, end) {
            if let Token::Qualified { right, .. } = &self.tokens[prim_index] {
                if let Some(primitive) = recognise(right) {
                    let node = self.emit_primitive(primitive, prim_index)?;
                    self.sequence(frontier, node);
                    return Ok(Subject::Primitive(Region {
                        head: Some(node),
                        tail: vec![node],
                        primitives: 1,
                    }));
                }
            }
        }
        // 2. `case <local_fn>(...)` — follow inline; a primitive-bearing call is
        //    a durable subject, a primitive-free call collapses to a data case.
        if let Some(name) = leading_local_call(self.tokens, start, end) {
            if self.functions.contains_key(&name) {
                let call = self.follow_named(&name, frontier, stack)?;
                if call.primitives > 0 {
                    return Ok(Subject::Primitive(call));
                }
            }
        }
        // 3. A non-primitive, non-primitive-bearing subject is data.
        Ok(Subject::Data)
    }

    /// Walks a named local function inline, pruning it if it carries no
    /// primitive. A recursive call (the function already on the stack) yields an
    /// empty region — the recursion is not flattened and not followed forever.
    fn follow_named(
        &mut self,
        name: &str,
        frontier: &Frontier,
        stack: &mut Vec<String>,
    ) -> Result<Region, StructureError> {
        if stack.iter().any(|frame| frame == name) {
            return Ok(Region::empty(frontier.clone()));
        }
        let Some(callee) = self.functions.get(name).copied() else {
            return Ok(Region::empty(frontier.clone()));
        };
        stack.push(name.to_owned());
        let region = self.walk_pruned(callee, frontier.clone(), stack)?;
        stack.pop();
        Ok(region)
    }

    /// Emits an `Opaque` node for an unbounded/unresolvable `case`, sequenced
    /// from the frontier. A primitive count of one keeps it from being pruned —
    /// an unmodellable shape is surfaced loudly, never silently dropped.
    fn opaque_region(&mut self, start: usize, end: usize, frontier: &Frontier) -> Region {
        let node = self.emit_opaque(start, end);
        self.sequence(frontier, node);
        Region {
            head: Some(node),
            tail: vec![node],
            primitives: 1,
        }
    }

    /// Whether the identifier at `index` begins a call to a known local function.
    fn is_local_call(&self, name: &str, index: usize) -> bool {
        self.functions.contains_key(name)
            && matches!(self.tokens.get(index + 1), Some(Token::OpenParen))
    }

    /// The absolute token index of the leading workflow-primitive call in the
    /// scrutinee `[start, end)`, or `None` if the subject is not a primitive.
    fn scrutinee_primitive(&self, start: usize, end: usize) -> Option<usize> {
        for index in start..end {
            if let Token::Qualified { left, right } = &self.tokens[index] {
                if *left == self.workflow_alias {
                    return recognise(right).map(|_| index);
                }
                // The first qualified call that is not a workflow primitive
                // means a data subject; stop scanning.
                return None;
            }
        }
        None
    }
}

/// Recognises a `aion/workflow` member name as a node primitive.
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

/// Maps every top-level `fn <name>(...) ... { <body> }` (with optional `pub`) to
/// its body's token range. A function definition is recognised by `fn` followed
/// by an identifier; the body is the brace-balanced block after the parameter
/// list and an optional `-> <return type>`.
fn map_functions(tokens: &[Token]) -> BTreeMap<String, FnBody> {
    let mut functions = BTreeMap::new();
    let mut index = 0;
    while index < tokens.len() {
        if matches!(&tokens[index], Token::Ident(word) if word == "fn") {
            if let Token::Ident(name) = tokens.get(index + 1).unwrap_or(&Token::Other(' ')) {
                if let Some(open) = find_open_brace(tokens, index + 2, tokens.len()) {
                    if let Some(close) = match_brace(tokens, open, tokens.len()) {
                        functions.insert(
                            name.clone(),
                            FnBody {
                                start: open + 1,
                                end: close,
                            },
                        );
                        index = close + 1;
                        continue;
                    }
                }
            }
        }
        index += 1;
    }
    functions
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn recognise_covers_the_vocabulary() {
        assert_eq!(recognise("run"), Some(NodePrimitive::Run));
        assert_eq!(
            recognise("spawn_and_wait"),
            Some(NodePrimitive::SpawnAndWait)
        );
        assert_eq!(recognise("now"), None);
    }

    #[test]
    fn map_functions_finds_bodies() {
        let tokens = super::super::scan::tokenise(
            "pub fn execute(input) { workflow.run(x) }\nfn helper() { ok }\n",
        );
        let map = map_functions(&tokens);
        assert!(map.contains_key("execute"));
        assert!(map.contains_key("helper"));
    }
}
