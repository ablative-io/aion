//! Guaranteed-bindings dataflow: which names a step's surface defines, and
//! the descending Kleene iteration that computes what is available on every
//! path into each step (`after` dependencies contribute conjunctively,
//! routing and fall-through predecessors disjunctively). A `collect` step
//! masks its region's per-instance names on the way out: only the collected
//! result crosses the region boundary.

use std::collections::{BTreeMap, BTreeSet};

use crate::Span;
use crate::ast::{ForkHeader, PipeEnd, Statement, Step};

use super::context::Flow;
use super::graph::{Provenance, RouteEdge};

/// Every name a step's surface binds for later steps.
pub(super) fn defined_names(step: &Step) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    defined_in_statements(&step.body, &mut names);
    names
}

pub(super) fn defined_in_statements(statements: &[Statement], names: &mut BTreeSet<String>) {
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
                if matches!(fork.header, ForkHeader::Named) {
                    defined_in_statements(&fork.body, names);
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
            Statement::Distribute(distribute) => {
                names.insert(distribute.var.clone());
            }
            Statement::Collect(collect) => {
                names.insert(collect.bind.name.clone());
            }
            Statement::SubStep(sub) => defined_in_statements(&sub.body, names),
            Statement::Spawn(_) | Statement::Sleep(_) | Statement::Route(_) => {}
        }
    }
}

pub(super) fn universe(flow: &Flow<'_>) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = flow.inputs.keys().cloned().collect();
    for step in flow.steps {
        names.extend(defined_names(step));
    }
    names
}

/// Where a name's value can come from at a program point: the set of
/// declaration sites still distinguishable, or `Unknown` when the origin is
/// unrecoverable (ambiguous writes within one step, unresolved `after`
/// resets, fixpoint seeds). A `Known` set with several members marks a real
/// graph join of distinct declarations — the walk reconciles their types
/// there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OriginSet {
    /// The declaration sites this name can arrive from.
    Known(BTreeSet<Span>),
    /// Ambiguous beyond recovery; absorbs every merge.
    Unknown,
}

impl OriginSet {
    fn single(span: Span) -> Self {
        Self::Known(BTreeSet::from([span]))
    }

    /// The unique declaration site, when exactly one remains.
    pub(super) fn unique(&self) -> Option<Span> {
        match self {
            Self::Known(spans) if spans.len() == 1 => spans.first().copied(),
            _ => None,
        }
    }

    /// The distinguishable declaration sites at a join (two or more).
    pub(super) fn joined(&self) -> Option<&BTreeSet<Span>> {
        match self {
            Self::Known(spans) if spans.len() > 1 => Some(spans),
            _ => None,
        }
    }
}

/// Declaration origins for names in scope at a program point.
pub(super) type Origins = BTreeMap<String, OriginSet>;

fn defined_origins(step: &Step) -> Origins {
    let mut origins = Origins::new();
    origins_in_statements(&step.body, &mut origins);
    origins
}

pub(super) fn origins_in_statements(statements: &[Statement], origins: &mut Origins) {
    for statement in statements {
        match statement {
            Statement::Call(call) => {
                if let Some(binding) = &call.bind {
                    insert_origin(origins, &binding.name, binding.span);
                }
            }
            Statement::Pipe(pipe) => {
                if let PipeEnd::Bind(binding) = &pipe.end {
                    insert_origin(origins, &binding.name, binding.span);
                }
            }
            Statement::Wait(wait) => insert_origin(origins, &wait.bind.name, wait.bind.span),
            Statement::Fork(fork) => {
                if matches!(fork.header, ForkHeader::Named) {
                    origins_in_statements(&fork.body, origins);
                }
                if let Some(binding) = &fork.join.bind {
                    insert_origin(origins, &binding.name, binding.span);
                }
            }
            Statement::Loop(looped) => {
                insert_origin(origins, &looped.var, looped.var_span);
                if let Some(counter) = &looped.counter {
                    insert_origin(origins, &counter.name, counter.span);
                }
            }
            Statement::Distribute(distribute) => {
                insert_origin(origins, &distribute.var, distribute.var_span);
            }
            Statement::Collect(collect) => {
                insert_origin(origins, &collect.bind.name, collect.bind.span);
            }
            Statement::SubStep(substep) => origins_in_statements(&substep.body, origins),
            Statement::Spawn(_) | Statement::Sleep(_) | Statement::Route(_) => {}
        }
    }
}

fn insert_origin(origins: &mut Origins, name: &str, span: Span) {
    // Two writes of one name WITHIN a step are sequential (last wins), not
    // a graph join — the origin is ambiguous, never a reconciliation site.
    match origins.get(name) {
        Some(OriginSet::Known(spans)) if spans.len() == 1 && spans.contains(&span) => {}
        Some(_) => {
            origins.insert(name.to_owned(), OriginSet::Unknown);
        }
        None => {
            origins.insert(name.to_owned(), OriginSet::single(span));
        }
    }
}

/// Descending Kleene iteration for the guaranteed-bindings dataflow.
/// `masks` subtracts a `collect` step's region-local names from its
/// outgoing set (the per-item track is merged there).
pub(super) fn availability(
    flow: &Flow<'_>,
    after: &[Vec<usize>],
    after_unknown: &[bool],
    routes: &[RouteEdge],
    fall_pred: &[Option<usize>],
    masks: &BTreeMap<usize, BTreeSet<String>>,
) -> (Vec<BTreeSet<String>>, Vec<Origins>) {
    let steps = flow.steps;
    let count = steps.len();
    let inputs: BTreeSet<String> = flow.inputs.keys().cloned().collect();
    let all = universe(flow);
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
            // An `after`-armed step has a first arrival carrying only its
            // dependencies' guarantees: that arming is one of the paths the
            // disjunctive meet ranges over, so a backward route into the
            // step cannot smuggle its bindings onto the first pass.
            let arming = (!after[position].is_empty()).then(|| incoming.clone());
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
            if let Some(arming) = &arming {
                merge(arming);
            }
            for edge in routes.iter().filter(|edge| edge.target == position) {
                match &edge.provenance {
                    Provenance::Success => merge(&avail_out[edge.source]),
                    // Compensation runs from the failed step's ENTRY set —
                    // the body's bindings never happened on this path — plus
                    // whatever the compensation bound before the route.
                    Provenance::Failure { defines, .. } => {
                        let mut contribution = avail_in[edge.source].clone();
                        contribution.extend(defines.iter().cloned());
                        merge(&contribution);
                    }
                }
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
            let mut passed = incoming.clone();
            if let Some(mask) = masks.get(&position) {
                for name in mask {
                    passed.remove(name);
                }
            }
            let outgoing: BTreeSet<String> = passed.union(&defined[position]).cloned().collect();
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
    let origins = origin_availability(
        flow,
        after,
        after_unknown,
        routes,
        fall_pred,
        masks,
        &avail_in,
    );
    (avail_in, origins)
}

fn origin_availability(
    flow: &Flow<'_>,
    after: &[Vec<usize>],
    after_unknown: &[bool],
    routes: &[RouteEdge],
    fall_pred: &[Option<usize>],
    masks: &BTreeMap<usize, BTreeSet<String>>,
    avail_in: &[BTreeSet<String>],
) -> Vec<Origins> {
    let count = flow.steps.len();
    let mut inputs = Origins::new();
    for (name, span) in &flow.input_origins {
        insert_origin(&mut inputs, name, *span);
    }
    let defined: Vec<Origins> = flow.steps.iter().map(defined_origins).collect();
    let mut origins_in: Vec<Origins> = avail_in
        .iter()
        .map(|names| {
            names
                .iter()
                .map(|name| (name.clone(), OriginSet::Unknown))
                .collect()
        })
        .collect();
    let mut origins_out = origins_in.clone();
    let mut changed = true;
    while changed {
        changed = false;
        for position in 0..count {
            let mut incoming = inputs.clone();
            for &dependency in &after[position] {
                merge_origins(&mut incoming, &origins_out[dependency]);
            }
            // The `after` arming path, mirroring `availability`.
            let arming = (!after[position].is_empty()).then(|| incoming.clone());
            merge_disjunctive(
                &mut incoming,
                position,
                arming.as_ref(),
                routes,
                fall_pred,
                &inputs,
                &OriginFlows {
                    ins: &origins_in,
                    outs: &origins_out,
                },
            );
            incoming.retain(|name, _| avail_in[position].contains(name));
            if after_unknown[position] {
                incoming = avail_in[position]
                    .iter()
                    .map(|name| (name.clone(), OriginSet::Unknown))
                    .collect();
            }
            let mut outgoing = incoming.clone();
            if let Some(mask) = masks.get(&position) {
                for name in mask {
                    outgoing.remove(name);
                }
            }
            for (name, origin) in &defined[position] {
                outgoing.insert(name.clone(), origin.clone());
            }
            if incoming != origins_in[position] {
                origins_in[position] = incoming;
                changed = true;
            }
            if outgoing != origins_out[position] {
                origins_out[position] = outgoing;
                changed = true;
            }
        }
    }
    origins_in
}

/// The per-step origin sets of the running fixpoint, borrowed together.
struct OriginFlows<'a> {
    ins: &'a [Origins],
    outs: &'a [Origins],
}

fn merge_disjunctive(
    incoming: &mut Origins,
    position: usize,
    arming: Option<&Origins>,
    routes: &[RouteEdge],
    fall_pred: &[Option<usize>],
    inputs: &Origins,
    flows: &OriginFlows<'_>,
) {
    let mut paths: Vec<Origins> = Vec::new();
    if position == 0 {
        paths.push(inputs.clone());
    }
    if let Some(arming) = arming {
        paths.push(arming.clone());
    }
    for edge in routes.iter().filter(|edge| edge.target == position) {
        match &edge.provenance {
            Provenance::Success => paths.push(flows.outs[edge.source].clone()),
            Provenance::Failure { origins, .. } => {
                let mut contribution = flows.ins[edge.source].clone();
                merge_origins(&mut contribution, origins);
                paths.push(contribution);
            }
        }
    }
    if let Some(predecessor) = fall_pred[position] {
        paths.push(flows.outs[predecessor].clone());
    }
    let Some(first_path) = paths.first() else {
        return;
    };
    for name in first_path.keys() {
        if !paths[1..].iter().all(|path| path.contains_key(name)) {
            continue;
        }
        // Union the declaration sites across the alternative paths; any
        // ambiguous contribution makes the whole join ambiguous.
        let mut union = OriginSet::Known(BTreeSet::new());
        for path in &paths {
            union = match (&union, path.get(name)) {
                (OriginSet::Known(existing), Some(OriginSet::Known(incoming))) => {
                    let mut spans = existing.clone();
                    spans.extend(incoming.iter().copied());
                    OriginSet::Known(spans)
                }
                _ => OriginSet::Unknown,
            };
            if union == OriginSet::Unknown {
                break;
            }
        }
        merge_origin(incoming, name, &union);
    }
}

fn merge_origins(target: &mut Origins, source: &Origins) {
    for (name, origin) in source {
        merge_origin(target, name, origin);
    }
}

fn merge_origin(target: &mut Origins, name: &str, origin: &OriginSet) {
    match (target.get_mut(name), origin) {
        (Some(OriginSet::Known(existing)), OriginSet::Known(incoming)) => {
            existing.extend(incoming.iter().copied());
        }
        (Some(existing @ OriginSet::Known(_)), OriginSet::Unknown) => {
            *existing = OriginSet::Unknown;
        }
        (Some(OriginSet::Unknown), _) => {}
        (None, _) => {
            target.insert(name.to_owned(), origin.clone());
        }
    }
}
