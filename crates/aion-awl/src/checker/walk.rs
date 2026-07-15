//! Flow walk: statements checked in written order, bindings threaded along
//! the graph. The walk runs three passes per flow — two silent passes seed
//! the global binding-type map (backward routes may read bindings declared
//! later in the file), the final pass emits diagnostics.

use std::collections::{BTreeMap, BTreeSet};

use crate::Span;
use crate::ast::{PipeEnd, PipeStmt, Statement, Step};

use super::blocks::{walk_collect, walk_distribute, walk_fork, walk_loop};
use super::calls::{check_max_visits, walk_call, walk_spawn, walk_wait};
use super::context::{Ctx, Flow};
use super::exprs::View;
use super::graph::StepGraph;
use super::outcomes::{Env, check_clauses, check_route};
use super::stages::walk_pipe;
use super::types::{Ty, assignable, same_ty};

/// A checker-scoped value and its uniquely resolved declaration.
#[derive(Clone)]
pub(super) struct ScopedTy {
    pub(super) ty: Ty,
    pub(super) declaration: Option<Span>,
}

/// The value environment used by the existing flow checker.
pub(super) type Scope = BTreeMap<String, ScopedTy>;

/// One active `loop`, for the sanctioned threaded-value rebinding.
pub(super) struct LoopFrame {
    /// The threaded name.
    pub(super) var: String,
    /// The seed's type; every rebind must stay assignable to it.
    pub(super) seed_ty: Ty,
    /// Whether the loop body rebound the threaded name at loop-body scope.
    pub(super) rebound: bool,
    /// The fork-branch nesting depth the loop was declared at: a bind made
    /// deeper (inside a collection-fork branch, whose bindings never
    /// escape) is not a rebind of the threaded value.
    pub(super) fork_depth: usize,
}

/// The flow-walk state for one pass.
pub(super) struct Walker<'c, 'a> {
    /// Shared checking context and tables.
    pub(super) ctx: &'c mut Ctx<'a>,
    /// The flow being walked (its steps, inputs, and outcomes).
    pub(super) flow: &'c Flow<'a>,
    /// Bindings from the previous pass.
    pub(super) prior: Scope,
    /// Bindings collected this pass.
    pub(super) next: Scope,
    /// Every write's type keyed by its declaration site — the join
    /// reconciliation reads incoming types per origin, so disjoint reuses
    /// of one name never contaminate each other.
    pub(super) by_decl: BTreeMap<(String, Span), Ty>,
    /// Whether diagnostics are recorded (final pass only).
    pub(super) emit: bool,
    /// Stack of active loops.
    pub(super) loops: Vec<LoopFrame>,
    /// Current collection-fork branch nesting depth.
    pub(super) fork_depth: usize,
    /// Whether the step being walked is a member of a route cycle: such a
    /// step may rebind a name that enters the cycle (the step-cycle
    /// analogue of the loop's threaded value), keeping its type.
    pub(super) cycle_member: bool,
    /// Names guaranteed INTO the current step (rebind candidates).
    pub(super) step_base: BTreeSet<String>,
    /// Names already rebound by the current step (once per step).
    pub(super) rebound: BTreeSet<String>,
    /// Region-local names that fall out of scope at the current step's
    /// `collect` (set only on collect steps).
    pub(super) collect_mask: Option<BTreeSet<String>>,
    /// The top-level step currently being walked (merge diagnostics).
    pub(super) step_name: String,
}

impl Walker<'_, '_> {
    /// Record a diagnostic on the emitting pass.
    pub(super) fn err(&mut self, span: Span, message: impl Into<String>) {
        if self.emit {
            self.ctx.error(span, message);
        }
    }

    /// Run `probe` with diagnostics suppressed.
    pub(super) fn silently<T>(&mut self, probe: impl FnOnce(&mut Self) -> T) -> T {
        let emitting = self.emit;
        self.emit = false;
        let result = probe(self);
        self.emit = emitting;
        result
    }
}

/// Run the flow walk over every step of one flow.
pub(super) fn run<'a>(ctx: &mut Ctx<'a>, flow: &Flow<'a>, graph: &StepGraph) {
    let mut prior = Scope::new();
    let mut by_decl: BTreeMap<(String, Span), Ty> = BTreeMap::new();
    for pass in 0..3 {
        let mut walker = Walker {
            ctx,
            flow,
            prior,
            next: Scope::new(),
            by_decl,
            emit: pass == 2,
            loops: Vec::new(),
            fork_depth: 0,
            cycle_member: false,
            step_base: BTreeSet::new(),
            rebound: BTreeSet::new(),
            collect_mask: None,
            step_name: String::new(),
        };
        for (position, step) in flow.steps.iter().enumerate() {
            walk_step(&mut walker, graph, position, step);
        }
        prior = walker.next;
        by_decl = walker.by_decl;
    }
}

fn walk_step(w: &mut Walker<'_, '_>, graph: &StepGraph, position: usize, step: &Step) {
    let mut base = Scope::new();
    if let Some(avail) = graph.avail_in.get(position) {
        for name in avail {
            let origin = graph
                .origins_in
                .get(position)
                .and_then(|origins| origins.get(name));
            let declaration = origin.and_then(super::avail::OriginSet::unique);
            let ty = w
                .flow
                .inputs
                .get(name)
                .cloned()
                .or_else(|| entry_ty(w, step, name, origin, declaration))
                .unwrap_or(Ty::Unknown);
            base.insert(name.clone(), ScopedTy { ty, declaration });
        }
    }
    w.cycle_member = graph.cyclic.get(position).copied().unwrap_or(false);
    w.step_base = base.keys().cloned().collect();
    w.rebound.clear();
    w.collect_mask = graph.collect_masks.get(&position).cloned();
    w.step_name.clone_from(&step.name);
    check_max_visits(w, step);
    let mut scope = base.clone();
    walk_statements(w, &mut scope, &step.body, step, &Env::Top);
    if let Some(on_failure) = &step.on_failure {
        let mut compensation = base;
        walk_statements(w, &mut compensation, &on_failure.body, step, &Env::Top);
        let terminal = matches!(
            on_failure.body.last(),
            Some(
                Statement::Route(_)
                    | Statement::Pipe(PipeStmt {
                        end: PipeEnd::Route(_),
                        ..
                    })
            )
        );
        if !terminal {
            w.err(
                on_failure.span,
                "an `on failure` block must end in a `route` — compensation cannot \
                 swallow the failure silently",
            );
        }
    }
    check_clauses(w, &scope, step, &Env::Top);
}

/// Walk statements in written order; returns the value the last statement
/// produces (a fork branch's result).
pub(super) fn walk_statements(
    w: &mut Walker<'_, '_>,
    scope: &mut Scope,
    statements: &[Statement],
    owner: &Step,
    env: &Env<'_>,
) -> Option<Ty> {
    let mut last = None;
    let mut routed_away: Option<bool> = None;
    for statement in statements {
        // `route` is illegal inside a `loop` body (ruled 2026-07-11): routes
        // connect steps and outcomes; a loop exits only through `until`/`max`.
        if !w.loops.is_empty()
            && let Some(span) = route_span_in_loop(statement)
        {
            w.err(
                span,
                "a `route` inside a `loop` body is illegal — a loop exits through its \
                 `until` condition (or the `max` ceiling), and routing happens in the \
                 step's outcome clauses after the loop",
            );
        }
        if routed_away == Some(false) {
            routed_away = Some(true);
            w.err(
                statement_span(statement),
                "unreachable statement — the `route` above always transfers control away",
            );
        }
        if routed_away.is_none() && is_unconditional_route(statement) {
            routed_away = Some(false);
        }
        last = walk_statement(w, scope, statement, statements, owner, env);
    }
    last
}

/// The span of a route this statement carries directly — a `route` line
/// anchors on the statement, a pipe chain on its `route` terminator's
/// target. Nested block statements return `None`: their inner statements
/// pass through this same check as the walk descends into them.
fn route_span_in_loop(statement: &Statement) -> Option<Span> {
    match statement {
        Statement::Route(route) => Some(route.span),
        Statement::Pipe(pipe) => match &pipe.end {
            PipeEnd::Route(target) => Some(target.span),
            PipeEnd::Bind(_) => None,
        },
        _ => None,
    }
}

/// Whether a statement unconditionally transfers control away (a `route`
/// line or a pipe chain terminating in `route`).
fn is_unconditional_route(statement: &Statement) -> bool {
    match statement {
        Statement::Route(_) => true,
        Statement::Pipe(pipe) => matches!(pipe.end, PipeEnd::Route(_)),
        _ => false,
    }
}

fn statement_span(statement: &Statement) -> Span {
    match statement {
        Statement::Call(call) => call.span,
        Statement::Spawn(spawn) => spawn.span,
        Statement::Pipe(pipe) => pipe.span,
        Statement::Wait(wait) => wait.span,
        Statement::Sleep(sleep) => sleep.span,
        Statement::Fork(fork) => fork.span,
        Statement::Loop(looped) => looped.span,
        Statement::Route(route) => route.span,
        Statement::SubStep(sub) => sub.span,
        Statement::Distribute(distribute) => distribute.span,
        Statement::Collect(collect) => collect.span,
    }
}

fn walk_statement(
    w: &mut Walker<'_, '_>,
    scope: &mut Scope,
    statement: &Statement,
    surrounding: &[Statement],
    owner: &Step,
    env: &Env<'_>,
) -> Option<Ty> {
    match statement {
        Statement::Call(call) => Some(walk_call(w, scope, call)),
        Statement::Spawn(spawn) => walk_spawn(w, scope, spawn),
        Statement::Pipe(pipe) => walk_pipe(w, scope, pipe, env),
        Statement::Wait(wait) => Some(walk_wait(w, scope, wait)),
        Statement::Sleep(_) => None,
        Statement::Fork(fork) => walk_fork(w, scope, fork, owner, env),
        Statement::Loop(looped) => walk_loop(w, scope, looped, owner, env),
        Statement::Distribute(distribute) => {
            walk_distribute(w, scope, distribute);
            None
        }
        Statement::Collect(collect) => {
            walk_collect(w, scope, collect);
            None
        }
        Statement::Route(route) => {
            let view = View {
                vars: scope,
                narrow: None,
                accessor: None,
            };
            check_route(w, view, &route.target, env, None);
            None
        }
        Statement::SubStep(sub) => {
            let siblings: Vec<String> = surrounding
                .iter()
                .filter_map(|candidate| match candidate {
                    Statement::SubStep(other) => Some(other.name.clone()),
                    _ => None,
                })
                .collect();
            let inner = Env::Substep {
                parent: owner,
                siblings,
            };
            check_max_visits(w, sub);
            walk_statements(w, scope, &sub.body, sub, &inner);
            check_clauses(w, scope, sub, &inner);
            None
        }
    }
}

pub(super) fn insert_binding(
    w: &mut Walker<'_, '_>,
    scope: &mut Scope,
    name: &str,
    ty: Ty,
    span: Span,
) {
    if w.emit {
        w.ctx.semantic.binding(span, name, &ty.to_string());
    }
    // Consts resolve anywhere an expression is legal; a binding of the same
    // name would make the reference ambiguous (and the fold unsound).
    if w.ctx.consts.contains_key(name) {
        w.err(
            span,
            format!("`{name}` is a document-level `const` — bindings cannot shadow consts"),
        );
    }
    // A bind inside a collection-fork branch never rebinds a loop's
    // threaded value: branch bindings do not escape the branch, so the
    // loop would still carry its old value into the next pass.
    let fork_depth = w.fork_depth;
    if let Some(frame) = w
        .loops
        .iter_mut()
        .rev()
        .find(|frame| frame.var == name && frame.fork_depth == fork_depth)
    {
        frame.rebound = true;
        let seed = frame.seed_ty.clone();
        if !assignable(&ty, &seed, &w.ctx.types) {
            w.err(
                span,
                format!(
                    "the loop threads `{name}` as {seed}; rebinding it as {ty} changes its type"
                ),
            );
        }
        scope.insert(
            name.to_owned(),
            ScopedTy {
                ty: seed,
                declaration: Some(span),
            },
        );
        return;
    }
    if let Some(existing) = scope.get(name).cloned() {
        // The second sanctioned rebinding: a step on a `max … visits` route
        // cycle rebinds a name that enters the cycle — each visit threads
        // the value forward (rev-3 §6: `fold` rebinds `state` and routes
        // back to `wave`). The type must survive the cycle re-entry: the
        // reference type is the value's type at step entry, falling back to
        // the upstream writer seen earlier this pass (the entry type merges
        // to `Unknown` across passes exactly when the types disagree).
        let sanctioned = w.cycle_member && w.step_base.contains(name) && !w.rebound.contains(name);
        if sanctioned {
            let reference = if matches!(existing.ty, Ty::Unknown) {
                w.next
                    .get(name)
                    .map(|entry| entry.ty.clone())
                    .filter(|entry| !matches!(entry, Ty::Unknown))
            } else {
                Some(existing.ty.clone())
            };
            if let Some(reference) = &reference
                && !assignable(&ty, reference, &w.ctx.types)
            {
                w.err(
                    span,
                    format!(
                        "rebinding `{name}` on a route cycle must keep its type: the \
                         cycle re-enters with {reference}, found {ty}"
                    ),
                );
            }
            w.rebound.insert(name.to_owned());
            let kept = ScopedTy {
                ty: reference.unwrap_or(ty),
                declaration: Some(span),
            };
            scope.insert(name.to_owned(), kept.clone());
            record_next(w, name, kept, span);
            return;
        }
        w.err(
            span,
            format!(
                "`{name}` is already bound — bindings are single-assignment per scope \
                 (the loop threaded value and a route-cycle step's re-entry rebinding \
                 are the two sanctioned exceptions)"
            ),
        );
    }
    let value = ScopedTy {
        ty,
        declaration: Some(span),
    };
    scope.insert(name.to_owned(), value.clone());
    record_next(w, name, value, span);
}

/// The type a name carries INTO a step, resolved per declaration origin.
/// A unique origin reads its own write's type (disjoint reuses of one name
/// never contaminate each other); a genuine graph join reconciles the
/// incoming origins' types — structurally equal types keep a concrete
/// representative, incompatible concrete types are a defect reported at the
/// joining step.
fn entry_ty(
    w: &mut Walker<'_, '_>,
    step: &Step,
    name: &str,
    origin: Option<&super::avail::OriginSet>,
    declaration: Option<Span>,
) -> Option<Ty> {
    if let Some(declaration) = declaration {
        return w
            .by_decl
            .get(&(name.to_owned(), declaration))
            .cloned()
            .or_else(|| w.prior.get(name).map(|value| value.ty.clone()));
    }
    if let Some(joined) = origin.and_then(super::avail::OriginSet::joined) {
        let incoming: Vec<Ty> = joined
            .iter()
            .filter_map(|site| w.by_decl.get(&(name.to_owned(), *site)).cloned())
            .filter(|ty| !matches!(ty, Ty::Unknown))
            .collect();
        if let Some(first) = incoming.first() {
            if let Some(conflicting) = incoming
                .iter()
                .find(|candidate| !same_ty(candidate, first, &w.ctx.types))
            {
                let step_name = step.name.clone();
                w.err(
                    step.name_span,
                    format!(
                        "`{name}` reaches step `{step_name}` as {first} on one path \
                         and as {conflicting} on another — paths that join must \
                         agree on a binding's type"
                    ),
                );
                return Some(Ty::Unknown);
            }
            return Some(first.clone());
        }
    }
    w.prior.get(name).map(|value| value.ty.clone())
}

/// Fold a binding into the cross-pass name table — a conservative fallback
/// for names whose origin is ambiguous; per-origin types live in `by_decl`
/// and joins reconcile through `entry_ty`.
fn record_next(w: &mut Walker<'_, '_>, name: &str, value: ScopedTy, span: Span) {
    let _ = span;
    if let Some(declaration) = value.declaration {
        w.by_decl
            .insert((name.to_owned(), declaration), value.ty.clone());
    }
    match w.next.get(name).cloned() {
        Some(existing) if existing.ty != value.ty || existing.declaration != value.declaration => {
            // The flow-wide cache is a conservative fallback only — real
            // reconciliation happens per origin at graph joins (`entry_ty`).
            // Structurally equal spellings keep a concrete representative;
            // genuinely different types degrade to `Unknown` here without a
            // diagnostic, because disjoint paths may legally reuse a name
            // (single assignment is per SCOPE, joined only by graph flow).
            let merged =
                if existing.ty == value.ty || same_ty(&existing.ty, &value.ty, &w.ctx.types) {
                    existing.ty
                } else {
                    Ty::Unknown
                };
            w.next.insert(
                name.to_owned(),
                ScopedTy {
                    ty: merged,
                    declaration: None,
                },
            );
        }
        _ => {
            w.next.insert(name.to_owned(), value);
        }
    }
}
