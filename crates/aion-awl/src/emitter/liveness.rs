//! Liveness analysis over the lowering plan's function nodes (regions and
//! substeps): refs/defs collection, route call edges, and the parameter
//! fixed-point `params(n) = (refs(n) ∪ ⋃ params(callee)) − defs(n)` —
//! iterated to stability because backward routes make the call graph cyclic.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{Expr, PipeEnd, RoutePayload, RouteTarget, Statement, Step};

use super::context::Emitter;
use super::error::EmitError;
use super::expr_refs::expr_refs;
use super::flowshape::{RegionShape, visits_counter};
use super::graph::{Node, Plan, Region, falls_through, substep_split};

/// The flow-shape context one flow's liveness runs under: its collapsed
/// per-item regions (with their already-planned member contracts) and its
/// exit, when the flow is itself a nested one.
pub(super) struct FlowLive<'f> {
    /// This flow's collapsed regions, by synthetic step name.
    pub(super) regions: &'f BTreeMap<String, RegionShape>,
    /// Member-flow plans computed bottom-up (by region id).
    pub(super) region_plans: &'f BTreeMap<usize, super::graph::NestedPlan>,
    /// The nested flow's exit, `None` for the host workflow.
    pub(super) exit: Option<ExitLive<'f>>,
}

/// A nested flow's exit contract, as liveness sees it.
#[derive(Clone, Copy)]
pub(super) enum ExitLive<'f> {
    /// A per-item region member flow: routing to (or falling into) the close
    /// step returns the collected binding.
    Region { name: &'f str, binding: &'f str },
    /// A subflow: routing to the outcome returns its payload.
    Subflow { name: &'f str },
}

/// Route-target resolution data: which liveness node a step-route calls.
struct Resolver<'r> {
    index: &'r BTreeMap<String, usize>,
    entry_region: &'r BTreeMap<usize, usize>,
    region_node: &'r [usize],
}

impl Resolver<'_> {
    /// The node a route target calls; `None` for workflow outcomes and
    /// parent outcome arms handled inline.
    fn step_route(&self, name: &str) -> Option<usize> {
        self.index
            .get(name)
            .and_then(|target| self.entry_region.get(target))
            .map(|&region| self.region_node[region])
    }
}

/// Liveness graph construction and the parameter fixed-point.
pub(super) fn build_params(
    emitter: &Emitter<'_>,
    steps: &[Step],
    regions: Vec<Region>,
    entry_region: BTreeMap<usize, usize>,
    index: &BTreeMap<String, usize>,
    live: &FlowLive<'_>,
) -> Result<Plan, EmitError> {
    let mut nodes: Vec<Node> = Vec::new();
    let mut region_node = Vec::new();
    let mut sub_node: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for _ in &regions {
        region_node.push(nodes.len());
        nodes.push(Node::default());
    }
    for (position, step) in steps.iter().enumerate() {
        let split = substep_split(step)?;
        for sub in split..step.body.len() {
            sub_node.insert((position, sub - split), nodes.len());
            nodes.push(Node::default());
        }
    }

    {
        let resolver = Resolver {
            index,
            entry_region: &entry_region,
            region_node: &region_node,
        };
        let mut liveness = Liveness {
            emitter,
            steps,
            nodes: &mut nodes,
            sub_node: &sub_node,
            resolver: &resolver,
            live,
        };
        for (region_position, region) in regions.iter().enumerate() {
            let node = region_node[region_position];
            for layer_members in &region.layers {
                for &member in layer_members {
                    liveness.collect_step(member, node)?;
                }
            }
        }
        // Fall-through at a region's end: an implicit tail call into the
        // next step's region (when one follows), or — in a per-item member
        // flow — the exit return of the collected binding.
        for (region_position, region) in regions.iter().enumerate() {
            let node = region_node[region_position];
            let Some(last) = region.layers.iter().flatten().copied().max() else {
                continue;
            };
            if !falls_through(&steps[last]) {
                continue;
            }
            let next = last + 1;
            if next < steps.len() {
                if let Some(&callee_region) = entry_region.get(&next) {
                    liveness.nodes[node]
                        .callees
                        .insert(region_node[callee_region]);
                }
            } else if let Some(ExitLive::Region { binding, .. }) = live.exit {
                if !liveness.nodes[node].defs.contains(binding) {
                    liveness.nodes[node].refs.insert(binding.to_owned());
                }
            }
        }
    }

    // Fixed point: params(n) = refs(n) ∪ (⋃ params(callee) − defs(n)).
    // `refs` is recorded in execution order (a name already defined at its
    // read site never registers), so a read-before-rebind still threads in
    // as a parameter — defs subtract only from callee needs.
    let mut params: Vec<BTreeSet<String>> = nodes.iter().map(|node| node.refs.clone()).collect();
    loop {
        let mut changed = false;
        for position in 0..nodes.len() {
            let mut wanted: BTreeSet<String> = params[position].clone();
            for &callee in &nodes[position].callees {
                for name in &params[callee] {
                    if !nodes[position].defs.contains(name) {
                        wanted.insert(name.clone());
                    }
                }
            }
            if wanted.len() != params[position].len() {
                params[position] = wanted;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    Ok(Plan {
        regions,
        entry_region,
        region_node,
        sub_node,
        params: params
            .into_iter()
            .map(|set| set.into_iter().collect())
            .collect(),
    })
}

/// One pass's working state while folding refs/defs/callees into nodes.
struct Liveness<'l, 'a> {
    emitter: &'l Emitter<'a>,
    steps: &'l [Step],
    nodes: &'l mut Vec<Node>,
    sub_node: &'l BTreeMap<(usize, usize), usize>,
    resolver: &'l Resolver<'l>,
    live: &'l FlowLive<'l>,
}

impl Liveness<'_, '_> {
    /// Fold one step's surface into its region node and substep nodes.
    fn collect_step(&mut self, member: usize, node: usize) -> Result<(), EmitError> {
        let step = &self.steps[member];
        // A bounded step reads its language-owned visit counter and its
        // bound expression at entry (before any of its defs).
        if let Some(max_visits) = &step.max_visits {
            let counter = visits_counter(step, &self.emitter.generated_names);
            if !self.nodes[node].defs.contains(&counter) {
                self.nodes[node].refs.insert(counter);
            }
            self.add_expr(&max_visits.bound, node, &BTreeSet::new());
        }
        // A collapsed per-item region step reads its collection and every
        // free name its member flow's wrapper threads in.
        if matches!(step.body.first(), Some(Statement::Distribute(_))) {
            let Some(region) = self.live.regions.get(&step.name) else {
                return Err(EmitError::new(
                    step.name_span,
                    format!("step `{}` lost its region shape", step.name),
                ));
            };
            let Some(nested) = self.live.region_plans.get(&region.id) else {
                return Err(EmitError::new(
                    step.name_span,
                    format!("step `{}` has no planned member flow", step.name),
                ));
            };
            for name in &nested.wrapper_params {
                if name != &region.var && !self.nodes[node].defs.contains(name) {
                    self.nodes[node].refs.insert(name.clone());
                }
            }
        }
        let split = substep_split(step)?;
        self.collect_statements(&step.body[..split], node);
        if let Some(on_failure) = &step.on_failure {
            self.collect_statements(&on_failure.body, node);
        }
        let sub_count = step.body.len() - split;
        if sub_count == 0 {
            for clause in &step.outcomes {
                self.collect_clause(clause, node);
                if let Some(callee) = self.resolver.step_route(&clause.route.name) {
                    self.nodes[node].callees.insert(callee);
                }
            }
            return Ok(());
        }
        // The region calls the first substep; substeps chain.
        let first = self.sub_node[&(member, 0)];
        self.nodes[node].callees.insert(first);
        for sub in 0..sub_count {
            self.collect_substep(member, split, sub, sub_count);
        }
        Ok(())
    }

    fn collect_substep(&mut self, member: usize, split: usize, sub: usize, sub_count: usize) {
        let step = &self.steps[member];
        let sub_id = self.sub_node[&(member, sub)];
        let Statement::SubStep(inner) = &step.body[split + sub] else {
            return;
        };
        self.collect_statements(&inner.body, sub_id);
        if let Some(on_failure) = &inner.on_failure {
            self.collect_statements(&on_failure.body, sub_id);
        }
        for clause in &inner.outcomes {
            self.collect_clause(clause, sub_id);
            self.sub_route_edges(member, split, &clause.route, sub_id);
        }
        let mut targets = Vec::new();
        collect_route_targets(&inner.body, &mut targets);
        if let Some(on_failure) = &inner.on_failure {
            collect_route_targets(&on_failure.body, &mut targets);
        }
        for target in targets {
            self.sub_route_edges(member, split, target, sub_id);
        }
        if falls_through(inner) {
            if sub + 1 < sub_count {
                let next = self.sub_node[&(member, sub + 1)];
                self.nodes[sub_id].callees.insert(next);
            } else {
                // Parent outcomes evaluate inline at chain end.
                for clause in &step.outcomes {
                    self.collect_clause(clause, sub_id);
                    if let Some(callee) = self.resolver.step_route(&clause.route.name) {
                        self.nodes[sub_id].callees.insert(callee);
                    }
                }
            }
        }
    }

    /// Add a substep's route edges (siblings, parent arms, workflow
    /// outcomes) to the liveness graph.
    fn sub_route_edges(
        &mut self,
        parent: usize,
        split: usize,
        target: &crate::ast::RouteTarget,
        sub_id: usize,
    ) {
        let step = &self.steps[parent];
        // Sibling substep?
        for (position, statement) in step.body[split..].iter().enumerate() {
            if let Statement::SubStep(candidate) = statement
                && candidate.name == target.name
            {
                let callee = self.sub_node[&(parent, position)];
                self.nodes[sub_id].callees.insert(callee);
                return;
            }
        }
        // Parent outcome arm?
        if let Some(clause) = step
            .outcomes
            .iter()
            .find(|clause| clause.name == target.name)
        {
            let mut refs = BTreeSet::new();
            self.target_refs(&clause.route, &mut refs);
            for name in refs {
                if !self.nodes[sub_id].defs.contains(&name) {
                    self.nodes[sub_id].refs.insert(name);
                }
            }
            if let Some(callee) = self.resolver.step_route(&clause.route.name) {
                self.nodes[sub_id].callees.insert(callee);
            }
            return;
        }
        // Workflow outcome or top-level step.
        if self.emitter.outcomes.contains_key(target.name.as_str()) {
            return;
        }
        if let Some(callee) = self.resolver.step_route(&target.name) {
            self.nodes[sub_id].callees.insert(callee);
        }
    }

    /// Fold one outcome clause's guard and payload references into a node.
    fn collect_clause(&mut self, clause: &crate::ast::OutcomeClause, node: usize) {
        let mut refs = BTreeSet::new();
        if let crate::ast::Guard::When { expr, .. } = &clause.guard {
            expr_refs(expr, &mut refs);
        }
        self.target_refs(&clause.route, &mut refs);
        for name in refs {
            if !self.nodes[node].defs.contains(&name) {
                self.nodes[node].refs.insert(name);
            }
        }
    }

    /// The names a route target's payload (or bare-route pickup) reads:
    /// constructed args, a value payload's expression, the exit contract
    /// (a member flow's collected binding; a subflow's bare pickup), or the
    /// binding named after a bare workflow outcome.
    fn target_refs(&self, target: &RouteTarget, refs: &mut BTreeSet<String>) {
        self.target_refs_form(target, refs, false);
    }

    /// [`target_refs`] for a PIPED route: the piped value is the payload, so
    /// the bare-route binding pickup never applies.
    fn piped_target_refs(&self, target: &RouteTarget, refs: &mut BTreeSet<String>) {
        self.target_refs_form(target, refs, true);
    }

    fn target_refs_form(&self, target: &RouteTarget, refs: &mut BTreeSet<String>, piped: bool) {
        for arg in target.payload_args() {
            expr_refs(&arg.value, refs);
        }
        if let Some(RoutePayload::Value(value)) = &target.payload {
            expr_refs(value, refs);
        }
        match self.live.exit {
            Some(ExitLive::Region { name, binding }) if target.name == name => {
                refs.insert(binding.to_owned());
                return;
            }
            Some(ExitLive::Subflow { name }) if target.name == name => {
                if target.payload.is_none() && !piped {
                    refs.insert(target.name.clone());
                }
                return;
            }
            _ => {}
        }
        if target.payload.is_none()
            && !piped
            && self.emitter.outcomes.contains_key(target.name.as_str())
        {
            // A bare route to a workflow outcome picks up the binding named
            // after the outcome, unless the payload type is Nil.
            refs.insert(target.name.clone());
        }
    }

    /// Walk statements collecting refs/defs into a node.
    fn collect_statements(&mut self, statements: &[Statement], node: usize) {
        let mut local = BTreeSet::new();
        self.collect_into(statements, node, &mut local);
        for name in local {
            self.nodes[node].defs.insert(name);
        }
    }

    fn add_expr(&mut self, expr: &Expr, node: usize, defined: &BTreeSet<String>) {
        let mut refs = BTreeSet::new();
        expr_refs(expr, &mut refs);
        for name in refs {
            if !defined.contains(&name) && !self.nodes[node].defs.contains(&name) {
                self.nodes[node].refs.insert(name);
            }
        }
    }

    fn collect_into(
        &mut self,
        statements: &[Statement],
        node: usize,
        defined: &mut BTreeSet<String>,
    ) {
        for statement in statements {
            match statement {
                Statement::Call(call) => {
                    for arg in &call.call.args {
                        self.add_expr(&arg.value, node, defined);
                    }
                    if let Some(bind) = &call.bind {
                        defined.insert(bind.name.clone());
                    }
                }
                Statement::Spawn(spawn) => {
                    for arg in &spawn.call.args {
                        self.add_expr(&arg.value, node, defined);
                    }
                }
                Statement::Pipe(pipe) => self.collect_pipe(pipe, node, defined),
                Statement::Wait(wait) => {
                    defined.insert(wait.bind.name.clone());
                }
                Statement::Fork(fork) => self.collect_fork(fork, node, defined),
                Statement::Loop(looped) => self.collect_loop(looped, node, defined),
                Statement::Route(route) => self.collect_route(route, node, defined),
                // The fan-out pair of a collapsed region step: the header
                // reads the collection, the collect defines the gathered
                // binding (member free names are folded in `collect_step`).
                Statement::Distribute(distribute) => {
                    self.add_expr(&distribute.collection, node, defined);
                }
                Statement::Collect(collect) => {
                    defined.insert(collect.bind.name.clone());
                }
                Statement::Sleep(_) | Statement::SubStep(_) => {}
            }
        }
        // Names defined here are defs of the node.
        for name in defined.iter() {
            self.nodes[node].defs.insert(name.clone());
        }
    }

    fn collect_pipe(
        &mut self,
        pipe: &crate::ast::PipeStmt,
        node: usize,
        defined: &mut BTreeSet<String>,
    ) {
        self.add_expr(&pipe.head, node, defined);
        for stage in &pipe.stages {
            if let crate::ast::PipeStage::Combinator(combinator) = stage
                && let Some(arg) = &combinator.arg
            {
                self.add_expr(arg, node, defined);
            }
        }
        match &pipe.end {
            PipeEnd::Bind(binding) => {
                defined.insert(binding.name.clone());
            }
            PipeEnd::Route(target) => {
                let mut refs = BTreeSet::new();
                self.piped_target_refs(target, &mut refs);
                for name in refs {
                    if !defined.contains(&name) && !self.nodes[node].defs.contains(&name) {
                        self.nodes[node].refs.insert(name);
                    }
                }
                if let Some(callee) = self.resolver.step_route(&target.name) {
                    self.nodes[node].callees.insert(callee);
                }
            }
        }
    }

    fn collect_fork(
        &mut self,
        fork: &crate::ast::ForkStmt,
        node: usize,
        defined: &mut BTreeSet<String>,
    ) {
        if let crate::ast::ForkHeader::Collection {
            var, collection, ..
        } = &fork.header
        {
            self.add_expr(collection, node, defined);
            let mut branch_defs = defined.clone();
            branch_defs.insert(var.clone());
            self.collect_into(&fork.body, node, &mut branch_defs);
        } else {
            // Named branches: bindings merge into the step at join.
            self.collect_into(&fork.body, node, defined);
        }
        if let Some(bind) = &fork.join.bind {
            defined.insert(bind.name.clone());
        }
    }

    fn collect_loop(
        &mut self,
        looped: &crate::ast::LoopStmt,
        node: usize,
        defined: &mut BTreeSet<String>,
    ) {
        self.add_expr(&looped.seed, node, defined);
        if let Some(max) = &looped.max {
            self.add_expr(&max.expr, node, defined);
        }
        let mut loop_defs = defined.clone();
        loop_defs.insert(looped.var.clone());
        if let Some(counter) = &looped.counter {
            loop_defs.insert(counter.name.clone());
        }
        self.collect_into(&looped.body, node, &mut loop_defs);
        if let Some(until) = &looped.until {
            self.add_expr(&until.expr, node, &loop_defs);
        }
        // The threaded value and the counter escape the loop.
        defined.insert(looped.var.clone());
        if let Some(counter) = &looped.counter {
            defined.insert(counter.name.clone());
        }
    }

    fn collect_route(
        &mut self,
        route: &crate::ast::RouteStmt,
        node: usize,
        defined: &mut BTreeSet<String>,
    ) {
        let mut refs = BTreeSet::new();
        self.target_refs(&route.target, &mut refs);
        for name in refs {
            if !defined.contains(&name) && !self.nodes[node].defs.contains(&name) {
                self.nodes[node].refs.insert(name);
            }
        }
        if let Some(callee) = self.resolver.step_route(&route.target.name) {
            self.nodes[node].callees.insert(callee);
        }
    }
}

fn collect_route_targets<'a>(
    statements: &'a [Statement],
    found: &mut Vec<&'a crate::ast::RouteTarget>,
) {
    for statement in statements {
        match statement {
            Statement::Pipe(pipe) => {
                if let PipeEnd::Route(target) = &pipe.end {
                    found.push(target);
                }
            }
            Statement::Route(route) => found.push(&route.target),
            Statement::Fork(fork) => collect_route_targets(&fork.body, found),
            Statement::Loop(looped) => collect_route_targets(&looped.body, found),
            Statement::Call(_)
            | Statement::Spawn(_)
            | Statement::Wait(_)
            | Statement::Sleep(_)
            | Statement::SubStep(_)
            | Statement::Distribute(_)
            | Statement::Collect(_) => {}
        }
    }
}
