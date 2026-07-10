//! Guaranteed-bindings dataflow: which names a step's surface defines, and
//! the descending Kleene iteration that computes what is available on every
//! path into each step (`after` dependencies contribute conjunctively,
//! routing and fall-through predecessors disjunctively).

use std::collections::BTreeSet;

use crate::ast::{ForkHeader, PipeEnd, Statement, Step};

use super::context::Ctx;
use super::graph::RouteEdge;

/// Every name a step's surface binds for later steps: call/pipe/wait binds,
/// loop threaded values and counters, join binds, named-branch binds, and
/// substep binds. `on failure` binds never escape.
pub(super) fn defined_names(step: &Step) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    defined_in_statements(&step.body, &mut names);
    names
}

fn defined_in_statements(statements: &[Statement], names: &mut BTreeSet<String>) {
    for statement in statements {
        match statement {
            Statement::Call(call) => {
                if let Some(bind) = &call.bind {
                    names.insert(bind.name.clone());
                }
            }
            Statement::Pipe(pipe) => {
                if let PipeEnd::Bind(bind) = &pipe.end {
                    names.insert(bind.name.clone());
                }
            }
            Statement::Wait(wait) => {
                names.insert(wait.bind.name.clone());
            }
            Statement::Fork(fork) => {
                match &fork.header {
                    ForkHeader::Collection { .. } => {}
                    ForkHeader::Named => {
                        defined_in_statements(&fork.body, names);
                    }
                }
                if let Some(bind) = &fork.join.bind {
                    names.insert(bind.name.clone());
                }
            }
            Statement::Loop(looped) => {
                names.insert(looped.var.clone());
                if let Some(counter) = &looped.counter {
                    names.insert(counter.name.clone());
                }
            }
            Statement::SubStep(sub) => {
                defined_in_statements(&sub.body, names);
            }
            Statement::Spawn(_) | Statement::Sleep(_) | Statement::Route(_) => {}
        }
    }
}

pub(super) fn universe(ctx: &Ctx<'_>) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = ctx.inputs.keys().cloned().collect();
    for step in &ctx.doc.steps {
        names.extend(defined_names(step));
    }
    names
}

/// Descending Kleene iteration for the guaranteed-bindings dataflow:
/// `after` dependencies contribute conjunctively (all complete), routing and
/// fall-through predecessors contribute disjunctively (intersection).
pub(super) fn availability(
    ctx: &Ctx<'_>,
    after: &[Vec<usize>],
    after_unknown: &[bool],
    routes: &[RouteEdge],
    fall_pred: &[Option<usize>],
) -> Vec<BTreeSet<String>> {
    let steps = &ctx.doc.steps;
    let count = steps.len();
    let inputs: BTreeSet<String> = ctx.inputs.keys().cloned().collect();
    let all = universe(ctx);
    let defined: Vec<BTreeSet<String>> = steps.iter().map(defined_names).collect();
    let mut avail_in: Vec<BTreeSet<String>> = vec![all.clone(); count];
    let mut avail_out: Vec<BTreeSet<String>> = vec![all.clone(); count];
    let mut changed = true;
    while changed {
        changed = false;
        for position in 0..count {
            let mut incoming = inputs.clone();
            for &dep in &after[position] {
                incoming.extend(avail_out[dep].iter().cloned());
            }
            let mut disjunctive: Option<BTreeSet<String>> = None;
            let mut merge = |set: &BTreeSet<String>| {
                disjunctive = Some(match disjunctive.take() {
                    None => set.clone(),
                    Some(previous) => previous.intersection(set).cloned().collect(),
                });
            };
            if position == 0 {
                merge(&inputs);
            }
            for edge in routes.iter().filter(|edge| edge.target == position) {
                merge(&avail_out[edge.source]);
            }
            if let Some(pred) = fall_pred[position] {
                merge(&avail_out[pred]);
            }
            if let Some(paths) = disjunctive {
                incoming.extend(paths);
            }
            if after_unknown[position] {
                incoming.clone_from(&all);
            }
            let outgoing: BTreeSet<String> = incoming.union(&defined[position]).cloned().collect();
            if incoming != avail_in[position] {
                avail_in[position] = incoming;
                changed = true;
            }
            if outgoing != avail_out[position] {
                avail_out[position] = outgoing;
                changed = true;
            }
        }
    }
    avail_in
}
