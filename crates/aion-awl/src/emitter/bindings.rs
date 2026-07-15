//! Global binding→type pass. AWL bindings are single-assignment along the
//! surface, so one workflow-wide map suffices — provided every use of a name
//! agrees on its type. Route-only steps may be written before the steps that
//! define what they read, so the pass iterates to a fixed point instead of
//! trusting written order; a name bound with two different types in disjoint
//! branches is refused with a spanned error (the map is keyed by name, so a
//! first-wins entry would mis-annotate the other branch's parameters).

use crate::Span;
use crate::ast::{ForkHeader, Statement, Step};

use super::context::Emitter;
use super::error::EmitError;
use super::exprs::{Scope, expr_type};
use super::pipes::stage_type;
use super::stmts::action_return;
use super::types::{GType, type_ref_to_g};

/// Populate `emitter.bindings` with every binding's type.
pub(super) fn compute(emitter: &mut Emitter<'_>) -> Result<(), EmitError> {
    for input in &emitter.document.inputs {
        emitter
            .bindings
            .insert(input.name.clone(), type_ref_to_g(&input.ty));
    }
    // Each pass can only add bindings; the surface is finite.
    loop {
        let scope = Scope::from_vars(emitter.bindings.clone());
        let mut discovered: Vec<(String, GType, Span)> = Vec::new();
        for step in &emitter.document.steps {
            collect_step(emitter, step, &scope, &mut discovered);
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

fn collect_step(
    emitter: &Emitter<'_>,
    step: &Step,
    scope: &Scope,
    discovered: &mut Vec<(String, GType, Span)>,
) {
    let mut local = scope.clone();
    for (name, ty, _) in discovered.iter() {
        local.entry(name.clone()).or_insert_with(|| ty.clone());
    }
    collect_statements(emitter, &step.body, &mut local, discovered);
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
            // Rev-3 region statements never reach the emitter (refused at
            // the entry gate before planning).
            Statement::Spawn(_)
            | Statement::Sleep(_)
            | Statement::Route(_)
            | Statement::Distribute(_)
            | Statement::Collect(_) => {}
        }
    }
}
