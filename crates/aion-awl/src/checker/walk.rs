//! Flow walk: statements checked in written order, bindings threaded along
//! the graph. The walk runs three passes — two silent passes seed the global
//! binding-type map (backward routes may read bindings declared later in the
//! file), the final pass emits diagnostics.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::Span;
use crate::ast::{
    CallStmt, ForkHeader, ForkStmt, LoopStmt, PipeEnd, PipeStmt, RetrySpec, SpawnStmt, Statement,
    Step, WaitStmt,
};
use crate::spanned::Spanned;

use super::context::Ctx;
use super::exprs::{View, check_args, type_of};
use super::graph::StepGraph;
use super::outcomes::{Env, check_clauses, check_route};
use super::stages::walk_pipe;
use super::types::{Ty, assignable, resolve};

/// One active `loop`, for the sanctioned threaded-value rebinding.
pub(super) struct LoopFrame {
    var: String,
    seed_ty: Ty,
    rebound: bool,
}

/// The flow-walk state for one pass.
pub(super) struct Walker<'c, 'a> {
    /// Shared checking context and tables.
    pub(super) ctx: &'c mut Ctx<'a>,
    /// Binding types from the previous pass (name → type).
    pub(super) prior: BTreeMap<String, Ty>,
    /// Binding types collected this pass.
    pub(super) next: BTreeMap<String, Ty>,
    /// Whether diagnostics are recorded (final pass only).
    pub(super) emit: bool,
    /// Stack of active loops.
    loops: Vec<LoopFrame>,
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

/// Run the flow walk over every step.
pub(super) fn run(ctx: &mut Ctx<'_>, graph: &StepGraph) {
    let mut prior: BTreeMap<String, Ty> = BTreeMap::new();
    for pass in 0..3 {
        let mut walker = Walker {
            ctx,
            prior,
            next: BTreeMap::new(),
            emit: pass == 2,
            loops: Vec::new(),
        };
        let doc = walker.ctx.doc;
        for (position, step) in doc.steps.iter().enumerate() {
            walk_step(&mut walker, graph, position, step);
        }
        prior = walker.next;
    }
}

fn walk_step(w: &mut Walker<'_, '_>, graph: &StepGraph, position: usize, step: &Step) {
    let mut base: BTreeMap<String, Ty> = BTreeMap::new();
    if let Some(avail) = graph.avail_in.get(position) {
        for name in avail {
            let ty = w
                .ctx
                .inputs
                .get(name)
                .or_else(|| w.prior.get(name))
                .cloned()
                .unwrap_or(Ty::Unknown);
            base.insert(name.clone(), ty);
        }
    }
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
    scope: &mut BTreeMap<String, Ty>,
    statements: &[Statement],
    owner: &Step,
    env: &Env<'_>,
) -> Option<Ty> {
    let mut last = None;
    for statement in statements {
        last = walk_statement(w, scope, statement, statements, owner, env);
    }
    last
}

fn walk_statement(
    w: &mut Walker<'_, '_>,
    scope: &mut BTreeMap<String, Ty>,
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
        Statement::Route(route) => {
            let view = View {
                vars: scope,
                narrow: None,
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
            walk_statements(w, scope, &sub.body, sub, &inner);
            check_clauses(w, scope, sub, &inner);
            None
        }
    }
}

pub(super) fn insert_binding(
    w: &mut Walker<'_, '_>,
    scope: &mut BTreeMap<String, Ty>,
    name: &str,
    ty: Ty,
    span: Span,
) {
    if let Some(frame) = w.loops.iter_mut().rev().find(|frame| frame.var == name) {
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
        scope.insert(name.to_owned(), seed);
        return;
    }
    if scope.contains_key(name) {
        w.err(
            span,
            format!(
                "`{name}` is already bound — bindings are single-assignment per scope \
                 (the loop threaded value is the one sanctioned rebinding)"
            ),
        );
    }
    scope.insert(name.to_owned(), ty.clone());
    match w.next.get(name) {
        Some(existing) if *existing != ty => {
            w.next.insert(name.to_owned(), Ty::Unknown);
        }
        _ => {
            w.next.insert(name.to_owned(), ty);
        }
    }
}

fn walk_call(w: &mut Walker<'_, '_>, scope: &mut BTreeMap<String, Ty>, call: &CallStmt) -> Ty {
    let view = View {
        vars: scope,
        narrow: None,
    };
    let name = &call.call.name;
    let returns = match w.ctx.callable(name).cloned() {
        None => {
            w.err(
                call.call.name_span,
                format!("no action or child named `{name}` is declared"),
            );
            Ty::Unknown
        }
        Some(callable) => {
            let kind = if w.ctx.actions.contains_key(name) {
                "action"
            } else {
                "child"
            };
            let params: Vec<(String, Ty)> = callable
                .params
                .iter()
                .map(|param| (param.name.clone(), param.ty.clone()))
                .collect();
            check_args(
                w,
                view,
                &call.call.args,
                &params,
                &format!("{kind} `{name}`"),
                "argument",
                call.call.name_span,
            );
            callable.returns
        }
    };
    if let Some(config) = &call.config
        && let Some(retry) = &config.retry
    {
        let span = match retry {
            RetrySpec::Every { span, .. } | RetrySpec::Backoff { span, .. } => *span,
        };
        w.err(
            span,
            "a call site may pin `node` and `timeout` only — `retry` stays on the \
             action declaration",
        );
    }
    if let Some(bind) = &call.bind {
        insert_binding(w, scope, &bind.name, returns.clone(), bind.span);
    }
    returns
}

fn walk_spawn(
    w: &mut Walker<'_, '_>,
    scope: &mut BTreeMap<String, Ty>,
    spawn: &SpawnStmt,
) -> Option<Ty> {
    let view = View {
        vars: scope,
        narrow: None,
    };
    let name = &spawn.call.name;
    if let Some(child) = w.ctx.children.get(name).cloned() {
        let params: Vec<(String, Ty)> = child
            .params
            .iter()
            .map(|param| (param.name.clone(), param.ty.clone()))
            .collect();
        check_args(
            w,
            view,
            &spawn.call.args,
            &params,
            &format!("child `{name}`"),
            "argument",
            spawn.call.name_span,
        );
    } else if w.ctx.actions.contains_key(name) {
        w.err(
            spawn.call.name_span,
            format!(
                "`{name}` is a worker action — `spawn` starts a declared child \
                 workflow; call the action directly instead"
            ),
        );
    } else {
        w.err(
            spawn.call.name_span,
            format!("`spawn` names an undeclared child workflow `{name}`"),
        );
    }
    if let Some(bind) = &spawn.bind {
        w.err(
            bind.span,
            "`spawn` is fire-and-forget — a spawned child cannot bind a result \
             (`->` after `spawn` is an error)",
        );
    }
    None
}

fn walk_wait(w: &mut Walker<'_, '_>, scope: &mut BTreeMap<String, Ty>, wait: &WaitStmt) -> Ty {
    let ty = if let Some(payload) = w.ctx.signals.get(&wait.signal).cloned() {
        payload
    } else {
        w.err(
            wait.signal_span,
            format!(
                "no signal named `{}` is declared in the workflow header",
                wait.signal
            ),
        );
        Ty::Unknown
    };
    let bound = if wait.timeout.is_some() {
        ty.optional()
    } else {
        ty
    };
    insert_binding(w, scope, &wait.bind.name, bound.clone(), wait.bind.span);
    bound
}

fn walk_fork(
    w: &mut Walker<'_, '_>,
    scope: &mut BTreeMap<String, Ty>,
    fork: &ForkStmt,
    owner: &Step,
    env: &Env<'_>,
) -> Option<Ty> {
    match &fork.header {
        ForkHeader::Collection {
            var,
            var_span,
            collection,
            ..
        } => {
            let view = View {
                vars: scope,
                narrow: None,
            };
            let collection_ty = type_of(w, view, collection);
            let element = match resolve(&collection_ty, &w.ctx.types) {
                Ty::List(inner) => (*inner).clone(),
                Ty::Unknown => Ty::Unknown,
                other => {
                    w.err(
                        collection.span(),
                        format!("`fork … in` needs a list to fan out over, found {other}"),
                    );
                    Ty::Unknown
                }
            };
            let mut branch = scope.clone();
            insert_binding(w, &mut branch, var, element, *var_span);
            let produced = walk_statements(w, &mut branch, &fork.body, owner, env);
            if let Some(bind) = &fork.join.bind {
                let joined = Ty::List(Rc::new(produced.unwrap_or(Ty::Unknown)));
                insert_binding(w, scope, &bind.name, joined, bind.span);
            }
        }
        ForkHeader::Named => {
            walk_statements(w, scope, &fork.body, owner, env);
            if let Some(bind) = &fork.join.bind {
                insert_binding(w, scope, &bind.name, Ty::Unknown, bind.span);
            }
        }
    }
    None
}

fn walk_loop(
    w: &mut Walker<'_, '_>,
    scope: &mut BTreeMap<String, Ty>,
    looped: &LoopStmt,
    owner: &Step,
    env: &Env<'_>,
) -> Option<Ty> {
    let view = View {
        vars: scope,
        narrow: None,
    };
    let seed_ty = type_of(w, view, &looped.seed);
    insert_binding(w, scope, &looped.var, seed_ty.clone(), looped.var_span);
    w.loops.push(LoopFrame {
        var: looped.var.clone(),
        seed_ty,
        rebound: false,
    });
    let mut body_scope = scope.clone();
    walk_statements(w, &mut body_scope, &looped.body, owner, env);
    let body_view = View {
        vars: &body_scope,
        narrow: None,
    };
    match &looped.until {
        Some(tail) => {
            let ty = type_of(w, body_view, &tail.expr);
            if !matches!(resolve(&ty, &w.ctx.types), Ty::Bool | Ty::Unknown) {
                w.err(
                    tail.expr.span(),
                    format!("`until` needs a Bool condition, found {ty}"),
                );
            }
        }
        None => {
            w.err(
                looped.span,
                "a loop must declare an `until` condition — the body must be able to stop",
            );
        }
    }
    match &looped.max {
        Some(tail) => {
            let ty = type_of(w, body_view, &tail.expr);
            if !matches!(resolve(&ty, &w.ctx.types), Ty::Int | Ty::Unknown) {
                w.err(
                    tail.expr.span(),
                    format!("`max` needs an Int ceiling, found {ty}"),
                );
            }
        }
        None => {
            w.err(
                looped.span,
                "this loop is unbounded — `max` is mandatory; unbounded \
                 `loop … until` is illegal",
            );
        }
    }
    if let Some(frame) = w.loops.pop()
        && !frame.rebound
    {
        w.err(
            looped.span,
            format!(
                "the loop body never rebinds `{}` — the threaded value must be \
                 rebound (`-> {}`) so the next pass sees it",
                looped.var, looped.var
            ),
        );
    }
    if let Some(counter) = &looped.counter {
        insert_binding(w, scope, &counter.name, Ty::Int, counter.span);
    }
    None
}
