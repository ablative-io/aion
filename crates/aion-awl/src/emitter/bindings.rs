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

/// Compute independent binding environments for the host and every nested flow.
pub(super) fn compute(emitter: &mut Emitter<'_>) -> Result<(), EmitError> {
    let mut host = BTreeMap::new();
    for input in &emitter.document.inputs {
        host.insert(input.name.clone(), type_ref_to_g(&input.ty));
    }
    let mut region_bindings = BTreeMap::new();
    compute_flow(
        emitter,
        &emitter.document.steps,
        emitter.host_regions,
        &mut host,
        &mut region_bindings,
    )?;
    emitter.bindings = host;

    for shape in emitter.subflow_shapes {
        let mut bindings = BTreeMap::new();
        for param in &shape.params {
            bindings.insert(param.name.clone(), type_ref_to_g(&param.ty));
        }
        compute_flow(
            emitter,
            &shape.flow.steps,
            &shape.flow.regions,
            &mut bindings,
            &mut region_bindings,
        )?;
        emitter
            .subflow_bindings
            .insert(shape.name.clone(), bindings);
    }
    emitter.region_bindings = region_bindings;
    Ok(())
}

fn compute_flow(
    emitter: &Emitter<'_>,
    steps: &[Step],
    regions: &BTreeMap<String, RegionShape>,
    bindings: &mut BTreeMap<String, GType>,
    region_bindings: &mut BTreeMap<usize, BTreeMap<String, GType>>,
) -> Result<(), EmitError> {
    register_counters(bindings, steps, &emitter.generated_names)?;
    loop {
        compute_region_maps(emitter, steps, regions, bindings, region_bindings)?;
        let scope = Scope::from_vars(bindings.clone());
        let mut discovered = Vec::new();
        collect_flow(
            emitter,
            steps,
            regions,
            region_bindings,
            &scope,
            &mut discovered,
        );
        let mut changed = false;
        for (name, ty, span) in discovered {
            match bindings.get(&name) {
                None => {
                    bindings.insert(name, ty);
                    changed = true;
                }
                Some(existing) if emitter.env.resolve(existing) != emitter.env.resolve(&ty) => {
                    return Err(EmitError::new(
                        span,
                        format!(
                            "`{name}` has incompatible types inside one flow: {} and {}",
                            emitter.env.gleam_type(&ty),
                            emitter.env.gleam_type(existing)
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

/// Compute extracted regions in fresh lexical environments. Region-owned
/// names shadow the enclosing flow; only each collected list is projected back.
fn compute_region_maps(
    emitter: &Emitter<'_>,
    steps: &[Step],
    regions: &BTreeMap<String, RegionShape>,
    enclosing: &BTreeMap<String, GType>,
    maps: &mut BTreeMap<usize, BTreeMap<String, GType>>,
) -> Result<(), EmitError> {
    let scope = Scope::from_vars(enclosing.clone());
    for step in steps {
        let Some(Statement::Distribute(distribute)) = step.body.first() else {
            continue;
        };
        let Some(region) = regions.get(&step.name) else {
            continue;
        };
        let Ok(GType::List(inner)) =
            expr_type(emitter, &distribute.collection, &scope).map(|ty| emitter.env.resolve(&ty))
        else {
            continue;
        };
        let mut local = enclosing.clone();
        remove_region_locals(&mut local, &region.members.steps, &emitter.generated_names)?;
        local.insert(region.var.clone(), (*inner).clone());
        compute_flow(
            emitter,
            &region.members.steps,
            &region.members.regions,
            &mut local,
            maps,
        )?;
        maps.insert(region.id, local);
    }
    Ok(())
}

/// Drop every region-owned binding from an enclosing seed. The names still
/// needed by member expressions remain as lexical free bindings.
fn remove_region_locals(
    bindings: &mut BTreeMap<String, GType>,
    steps: &[Step],
    names: &super::generated_names::GeneratedNames,
) -> Result<(), EmitError> {
    for step in steps {
        if step.max_visits.is_some() {
            bindings.remove(&visits_counter(step, names)?);
        }
        remove_statement_locals(bindings, &step.body);
        if let Some(on_failure) = &step.on_failure {
            remove_statement_locals(bindings, &on_failure.body);
        }
    }
    Ok(())
}

fn remove_statement_locals(bindings: &mut BTreeMap<String, GType>, statements: &[Statement]) {
    for statement in statements {
        match statement {
            Statement::Call(call) => {
                if let Some(bind) = &call.bind {
                    bindings.remove(&bind.name);
                }
            }
            Statement::Pipe(pipe) => {
                if let crate::ast::PipeEnd::Bind(bind) = &pipe.end {
                    bindings.remove(&bind.name);
                }
            }
            Statement::Wait(wait) => {
                bindings.remove(&wait.bind.name);
            }
            Statement::Fork(fork) => {
                if let ForkHeader::Collection { var, .. } = &fork.header {
                    bindings.remove(var);
                }
                if let Some(bind) = &fork.join.bind {
                    bindings.remove(&bind.name);
                }
                remove_statement_locals(bindings, &fork.body);
            }
            Statement::Loop(looped) => {
                bindings.remove(&looped.var);
                if let Some(counter) = &looped.counter {
                    bindings.remove(&counter.name);
                }
                remove_statement_locals(bindings, &looped.body);
            }
            Statement::SubStep(sub) => {
                remove_statement_locals(bindings, &sub.body);
                if let Some(on_failure) = &sub.on_failure {
                    remove_statement_locals(bindings, &on_failure.body);
                }
            }
            Statement::Collect(collect) => {
                bindings.remove(&collect.bind.name);
            }
            Statement::Spawn(_)
            | Statement::Sleep(_)
            | Statement::Route(_)
            | Statement::Distribute(_) => {}
        }
    }
}

/// Register the `Int` visit counter of every bounded step in a step list.
fn register_counters(
    bindings: &mut BTreeMap<String, GType>,
    steps: &[Step],
    names: &super::generated_names::GeneratedNames,
) -> Result<(), EmitError> {
    for step in steps {
        if step.max_visits.is_some() {
            bindings.insert(visits_counter(step, names)?, GType::Int);
        }
    }
    Ok(())
}

/// Walk one flow's steps, discovering binding types (recursing into region
/// member flows with the per-item variable in scope).
fn collect_flow(
    emitter: &Emitter<'_>,
    steps: &[Step],
    regions: &BTreeMap<String, RegionShape>,
    region_bindings: &BTreeMap<usize, BTreeMap<String, GType>>,
    scope: &Scope,
    discovered: &mut Vec<(String, GType, Span)>,
) {
    for step in steps {
        let mut local = scope.clone();
        for (name, ty, _) in discovered.iter() {
            local.entry(name.clone()).or_insert_with(|| ty.clone());
        }
        if let Some(Statement::Distribute(_)) = step.body.first() {
            let Some(region) = regions.get(&step.name) else {
                continue;
            };
            // The gathered collection is the sole value projected from the
            // isolated member environment into this enclosing flow.
            if let Some(Statement::Collect(collect)) = step.body.get(1)
                && let Some(item_ty) = region_bindings
                    .get(&region.id)
                    .and_then(|member| member.get(&region.binding))
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
