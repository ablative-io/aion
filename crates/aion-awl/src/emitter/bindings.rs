//! Global binding→type pass. AWL bindings are single-assignment along the
//! surface, so one workflow-wide map suffices — provided every use of a name
//! agrees on its type. Route-only steps may be written before the steps that
//! define what they read, so the pass iterates to a fixed point instead of
//! trusting written order; a name bound with two different types in disjoint
//! branches is refused with a spanned error (the map is keyed by name, so a
//! first-wins entry would mis-annotate the other branch's parameters).
//!
//! The rev-3 flow shape folds in here too: subflow invocations type as the
//! subflow's outcome, a collapsed region step types its collected binding
//! from the member flow (`[T]`, or `[T?]` for the tolerant form), and every
//! bounded step's language-owned visit counter registers as an `Int`.

use std::collections::BTreeMap;

use crate::Span;
use crate::ast::{ForkHeader, Statement, Step};

use super::context::Emitter;
use super::error::EmitError;
use super::exprs::{Scope, expr_type};
use super::flowshape::{RegionShape, visits_counter};
use super::pipes::stage_type;
use super::stmts::action_return;
use super::types::{GType, type_ref_to_g};

/// Populate `emitter.bindings` with every binding's type, across the host
/// flow, every subflow, and every per-item region member flow.
pub(super) fn compute(emitter: &mut Emitter<'_>) -> Result<(), EmitError> {
    for input in &emitter.document.inputs {
        emitter
            .bindings
            .insert(input.name.clone(), type_ref_to_g(&input.ty));
    }
    for shape in emitter.subflow_shapes {
        for param in &shape.params {
            emitter
                .bindings
                .insert(param.name.clone(), type_ref_to_g(&param.ty));
        }
    }
    let document = emitter.document;
    let subflow_shapes = emitter.subflow_shapes;
    let host_regions = emitter.host_regions;
    register_counters(emitter, &document.steps);
    for shape in subflow_shapes {
        register_counters(emitter, &shape.flow.steps);
        register_region_counters(emitter, &shape.flow.regions);
    }
    register_region_counters(emitter, host_regions);

    // Each pass can only add bindings; the surface is finite.
    loop {
        let scope = Scope::from_vars(emitter.bindings.clone());
        let mut discovered: Vec<(String, GType, Span)> = Vec::new();
        collect_flow(
            emitter,
            &emitter.document.steps,
            emitter.host_regions,
            &scope,
            &mut discovered,
        );
        for shape in emitter.subflow_shapes {
            collect_flow(
                emitter,
                &shape.flow.steps,
                &shape.flow.regions,
                &scope,
                &mut discovered,
            );
        }
        let mut changed = false;
        for (name, ty, span) in discovered {
            match emitter.bindings.get(&name) {
                None => {
                    emitter.bindings.insert(name, ty);
                    changed = true;
                }
                Some(existing) if emitter.env.resolve(existing) != emitter.env.resolve(&ty) => {
                    return Err(EmitError::new(
                        span,
                        format!(
                            "`{name}` is bound as {} here but as {} elsewhere in the \
                             workflow — the Gleam stopgap threads one type per binding \
                             name across branches",
                            emitter.env.gleam_type(&ty),
                            emitter.env.gleam_type(existing),
                        ),
                    ));
                }
                Some(_) => {}
            }
        }
        if !changed {
            return Ok(());
        }
    }
}

/// Register the `Int` visit counter of every bounded step in a step list.
fn register_counters(emitter: &mut Emitter<'_>, steps: &[Step]) {
    for step in steps {
        if step.max_visits.is_some() {
            emitter
                .bindings
                .insert(visits_counter(&step.name), GType::Int);
        }
    }
}

/// [`register_counters`] over a region map's member flows, recursively.
fn register_region_counters(emitter: &mut Emitter<'_>, regions: &BTreeMap<String, RegionShape>) {
    let mut names = Vec::new();
    region_counter_names(regions, &mut names);
    for name in names {
        emitter.bindings.insert(name, GType::Int);
    }
}

fn region_counter_names(regions: &BTreeMap<String, RegionShape>, out: &mut Vec<String>) {
    for region in regions.values() {
        for step in &region.members.steps {
            if step.max_visits.is_some() {
                out.push(visits_counter(&step.name));
            }
        }
        region_counter_names(&region.members.regions, out);
    }
}

/// Walk one flow's steps, discovering binding types (recursing into region
/// member flows with the per-item variable in scope).
fn collect_flow(
    emitter: &Emitter<'_>,
    steps: &[Step],
    regions: &BTreeMap<String, RegionShape>,
    scope: &Scope,
    discovered: &mut Vec<(String, GType, Span)>,
) {
    for step in steps {
        let mut local = scope.clone();
        for (name, ty, _) in discovered.iter() {
            local.entry(name.clone()).or_insert_with(|| ty.clone());
        }
        if let Some(Statement::Distribute(distribute)) = step.body.first() {
            let Some(region) = regions.get(&step.name) else {
                continue;
            };
            let elem = expr_type(emitter, &distribute.collection, &local)
                .map(|ty| emitter.env.resolve(&ty));
            if let Ok(GType::List(inner)) = &elem {
                // The per-item variable registers globally: member-flow
                // function parameters annotate through the bindings map.
                define(
                    &region.var,
                    (**inner).clone(),
                    distribute.var_span,
                    &mut local,
                    discovered,
                );
            }
            collect_flow(
                emitter,
                &region.members.steps,
                &region.members.regions,
                &local,
                discovered,
            );
            // The gathered collection: `[T]`, or `[T?]` for the tolerant
            // form, where `T` is the collected member binding's type.
            let mut member_local = local.clone();
            for (name, ty, _) in discovered.iter() {
                member_local
                    .entry(name.clone())
                    .or_insert_with(|| ty.clone());
            }
            if let Some(Statement::Collect(collect)) = step.body.get(1)
                && let Some(item_ty) = member_local.get(&region.binding)
            {
                let slot = if region.tolerant {
                    GType::Option(Box::new(item_ty.clone()))
                } else {
                    item_ty.clone()
                };
                define(
                    &collect.bind.name,
                    GType::List(Box::new(slot)),
                    collect.bind.span,
                    &mut local,
                    discovered,
                );
            }
            // The rest of the synthetic step's body (the close step's
            // remaining statements) walks below with the collect bound.
            collect_statements(emitter, &step.body[2..], &mut local, discovered);
            continue;
        }
        collect_statements(emitter, &step.body, &mut local, discovered);
    }
}

fn define(
    name: &str,
    ty: GType,
    span: Span,
    local: &mut Scope,
    discovered: &mut Vec<(String, GType, Span)>,
) {
    local.entry(name.to_owned()).or_insert_with(|| ty.clone());
    discovered.push((name.to_owned(), ty, span));
}

fn collect_statements(
    emitter: &Emitter<'_>,
    statements: &[Statement],
    local: &mut Scope,
    discovered: &mut Vec<(String, GType, Span)>,
) {
    for statement in statements {
        match statement {
            Statement::Call(call) => {
                if let Some(bind) = &call.bind
                    && let Some(returns) = action_return(emitter, &call.call.name)
                {
                    define(&bind.name, returns, bind.span, local, discovered);
                }
            }
            Statement::Wait(wait) => {
                if let Some(&signal) = emitter.signals.get(wait.signal.as_str()) {
                    let payload = type_ref_to_g(&signal.ty);
                    let ty = if wait.timeout.is_some() {
                        GType::Option(Box::new(payload))
                    } else {
                        payload
                    };
                    define(&wait.bind.name, ty, wait.bind.span, local, discovered);
                }
            }
            Statement::Pipe(pipe) => {
                if let crate::ast::PipeEnd::Bind(binding) = &pipe.end {
                    let Ok(mut current) = expr_type(emitter, &pipe.head, local) else {
                        continue;
                    };
                    let mut resolved = true;
                    for stage in &pipe.stages {
                        if let Ok(next) = stage_type(emitter, &current, stage) {
                            current = next;
                        } else {
                            resolved = false;
                            break;
                        }
                    }
                    if resolved {
                        define(&binding.name, current, binding.span, local, discovered);
                    }
                }
            }
            Statement::Fork(fork) => {
                match &fork.header {
                    ForkHeader::Collection {
                        var, collection, ..
                    } => {
                        let elem = expr_type(emitter, collection, local)
                            .map(|ty| emitter.env.resolve(&ty));
                        let mut branch_scope = local.clone();
                        if let Ok(GType::List(inner)) = &elem {
                            branch_scope.insert(var.clone(), (**inner).clone());
                        }
                        // Branch bindings stay branch-local; walk only for
                        // nested constructs' sake.
                        let mut branch_discovered = Vec::new();
                        collect_statements(
                            emitter,
                            &fork.body,
                            &mut branch_scope,
                            &mut branch_discovered,
                        );
                        if let (Some(bind), [Statement::Call(call)]) =
                            (&fork.join.bind, fork.body.as_slice())
                            && let Some(returns) = action_return(emitter, &call.call.name)
                        {
                            define(
                                &bind.name,
                                GType::List(Box::new(returns)),
                                bind.span,
                                local,
                                discovered,
                            );
                        }
                    }
                    ForkHeader::Named => {
                        collect_statements(emitter, &fork.body, local, discovered);
                    }
                }
            }
            Statement::Loop(looped) => {
                if let Ok(seed) = expr_type(emitter, &looped.seed, local) {
                    define(&looped.var, seed, looped.var_span, local, discovered);
                }
                if let Some(counter) = &looped.counter {
                    define(&counter.name, GType::Int, counter.span, local, discovered);
                }
                collect_statements(emitter, &looped.body, local, discovered);
            }
            Statement::SubStep(sub) => {
                collect_statements(emitter, &sub.body, local, discovered);
            }
            // Region markers type at the flow walk (`collect_flow`), which
            // knows the region shape; the rest bind nothing.
            Statement::Spawn(_)
            | Statement::Sleep(_)
            | Statement::Route(_)
            | Statement::Distribute(_)
            | Statement::Collect(_) => {}
        }
    }
}
