//! Bounded-loop lowering: a top-level tail-recursive Gleam function per
//! loop, threading the one sanctioned rebinding and a language-owned
//! counter (survey fix 1 / D3 — workers never carry the counter), plus the
//! statement refs/defs collectors the liveness analysis shares.

use std::collections::BTreeSet;

use crate::ast::{ForkHeader, LoopStmt, Statement};

use super::context::Emitter;
use super::error::EmitError;
use super::exprs::{Scope, expr_type, render_expr};
use super::graph::expr_refs;
use super::names::{ident, snake};
use super::stmts::flush_prelude;
use super::types::GType;

/// The statement-list lowering callback a loop body recurses through.
pub(super) type LowerBody<'c> =
    dyn FnMut(&mut Emitter<'_>, &[Statement], &mut Scope) -> Result<(), EmitError> + 'c;

/// Lower a bounded loop to a top-level tail-recursive function with a
/// language-owned counter (survey fix 1: workers never carry the counter).
pub(super) fn lower_loop(
    emitter: &mut Emitter<'_>,
    ctx_step: &str,
    looped: &LoopStmt,
    scope: &mut Scope,
    lower_body: &mut LowerBody<'_>,
) -> Result<(), EmitError> {
    let Some(max) = &looped.max else {
        return Err(EmitError::new(
            looped.span,
            "an unbounded loop (no `max`) is illegal until implicit continue-as-new lands",
        ));
    };
    let mut prelude = Vec::new();
    let seed = render_expr(emitter, &looped.seed, scope, &mut prelude)?;
    let seed_ty = expr_type(emitter, &looped.seed, scope)?;
    let max_rendered = render_expr(emitter, &max.expr, scope, &mut prelude)?;
    flush_prelude(emitter, prelude);

    let free = loop_free_names(looped, scope);
    let loop_fn = format!("{}_loop_{}", snake(ctx_step), emitter.loop_counter);
    emitter.loop_counter += 1;

    let var = ident(&looped.var);
    let counter_named = looped.counter.is_some();
    let result_ty = if counter_named {
        format!("#({}, Int)", emitter.env.gleam_type(&seed_ty))
    } else {
        emitter.env.gleam_type(&seed_ty)
    };
    let (comma_free, comma_annotated_free) = loop_param_lists(emitter, &free, scope);

    // Call site.
    let binder = if counter_named {
        format!(
            "#({var}, {})",
            ident(
                &looped
                    .counter
                    .as_ref()
                    .map(|c| c.name.clone())
                    .unwrap_or_default()
            )
        )
    } else {
        var.clone()
    };
    emitter.line(&format!(
        "use {binder} <- try({loop_fn}({seed}, 0, {max_rendered}{comma_free}))"
    ));

    // Loop function body.
    let mut loop_scope = scope.clone();
    loop_scope.insert(looped.var.clone(), seed_ty.clone());
    if let Some(counter) = &looped.counter {
        loop_scope.insert(counter.name.clone(), GType::Int);
    }
    let until = looped.until.as_ref().map(|tail| tail.expr.clone());
    let body = looped.body.clone();
    let rendered = emitter.capture(|this| {
        let var_annotation = this.env.gleam_type(&seed_ty);
        this.line(&format!(
            "fn {loop_fn}({var}: {var_annotation}, awl_count: Int, awl_max: \
             Int{comma_annotated_free}) -> Result({result_ty}, AwlError) {{"
        ));
        this.indented_try(|this| {
            let mut inner_scope = loop_scope.clone();
            lower_body(this, &body, &mut inner_scope)?;
            this.line("let awl_count = awl_count + 1");
            let exit = if counter_named {
                format!("Ok(#({var}, awl_count))")
            } else {
                format!("Ok({var})")
            };
            let recurse = format!("{loop_fn}({var}, awl_count, awl_max{comma_free})");
            let bound_check = |this: &mut Emitter<'_>| {
                this.line("case awl_count >= awl_max {");
                this.indented(|this| {
                    this.line(&format!("True -> {exit}"));
                    this.line(&format!("False -> {recurse}"));
                });
                this.line("}");
            };
            match &until {
                Some(condition) => {
                    let mut condition_prelude = Vec::new();
                    let rendered_condition =
                        render_expr(this, condition, &inner_scope, &mut condition_prelude)?;
                    flush_prelude(this, condition_prelude);
                    this.line(&format!("case {rendered_condition} {{"));
                    this.indented(|this| {
                        this.line(&format!("True -> {exit}"));
                        this.line("False ->");
                        this.indented(bound_check);
                    });
                    this.line("}");
                }
                None => bound_check(this),
            }
            Ok(())
        })?;
        this.line("}");
        Ok(())
    })?;
    emitter.loop_fns.push(rendered);

    scope.insert(looped.var.clone(), seed_ty);
    if let Some(counter) = &looped.counter {
        scope.insert(counter.name.clone(), GType::Int);
    }
    Ok(())
}

/// Free names a loop body and its `until` reference beyond the loop-locals:
/// these thread into the generated loop function as parameters.
fn loop_free_names(looped: &LoopStmt, scope: &Scope) -> Vec<String> {
    let mut refs = BTreeSet::new();
    statements_expr_refs(&looped.body, &mut refs);
    if let Some(until) = &looped.until {
        expr_refs(&until.expr, &mut refs);
    }
    let mut defs = BTreeSet::new();
    statement_defs(&looped.body, &mut defs);
    refs.remove(&looped.var);
    if let Some(counter) = &looped.counter {
        refs.remove(&counter.name);
    }
    refs.into_iter()
        .filter(|name| !defs.contains(name) && scope.contains_key(name))
        .collect()
}

/// The `, a, b` call-argument tail and its `, a: A, b: B` annotated twin.
fn loop_param_lists(emitter: &Emitter<'_>, free: &[String], scope: &Scope) -> (String, String) {
    if free.is_empty() {
        return (String::new(), String::new());
    }
    let args = free
        .iter()
        .map(|name| ident(name))
        .collect::<Vec<_>>()
        .join(", ");
    let annotated = free
        .iter()
        .map(|name| {
            let annotation = scope
                .get(name)
                .map_or_else(|| "Nil".to_owned(), |ty| emitter.env.gleam_type(ty));
            format!("{}: {annotation}", ident(name))
        })
        .collect::<Vec<_>>()
        .join(", ");
    (format!(", {args}"), format!(", {annotated}"))
}

/// Names referenced anywhere in a statement list's expressions.
pub(super) fn statements_expr_refs(statements: &[Statement], refs: &mut BTreeSet<String>) {
    for statement in statements {
        match statement {
            Statement::Call(call) => {
                for arg in &call.call.args {
                    expr_refs(&arg.value, refs);
                }
            }
            Statement::Spawn(spawn) => {
                for arg in &spawn.call.args {
                    expr_refs(&arg.value, refs);
                }
            }
            Statement::Pipe(pipe) => {
                expr_refs(&pipe.head, refs);
                if let crate::ast::PipeEnd::Route(target) = &pipe.end
                    && let Some(payload) = &target.payload
                {
                    for arg in payload {
                        expr_refs(&arg.value, refs);
                    }
                }
            }
            Statement::Wait(_) | Statement::Sleep(_) => {}
            Statement::Fork(fork) => {
                if let ForkHeader::Collection { collection, .. } = &fork.header {
                    expr_refs(collection, refs);
                }
                statements_expr_refs(&fork.body, refs);
            }
            Statement::Loop(looped) => {
                expr_refs(&looped.seed, refs);
                if let Some(max) = &looped.max {
                    expr_refs(&max.expr, refs);
                }
                if let Some(until) = &looped.until {
                    expr_refs(&until.expr, refs);
                }
                statements_expr_refs(&looped.body, refs);
            }
            Statement::Route(route) => {
                if let Some(payload) = &route.target.payload {
                    for arg in payload {
                        expr_refs(&arg.value, refs);
                    }
                }
            }
            Statement::SubStep(sub) => {
                statements_expr_refs(&sub.body, refs);
            }
        }
    }
}

/// Names a statement list defines (loop-var/counter escapes included, fork
/// collection-branch locals excluded).
pub(super) fn statement_defs(statements: &[Statement], defs: &mut BTreeSet<String>) {
    for statement in statements {
        match statement {
            Statement::Call(call) => {
                if let Some(bind) = &call.bind {
                    defs.insert(bind.name.clone());
                }
            }
            Statement::Pipe(pipe) => {
                if let crate::ast::PipeEnd::Bind(binding) = &pipe.end {
                    defs.insert(binding.name.clone());
                }
            }
            Statement::Wait(wait) => {
                defs.insert(wait.bind.name.clone());
            }
            Statement::Fork(fork) => {
                if matches!(fork.header, ForkHeader::Named) {
                    statement_defs(&fork.body, defs);
                }
                if let Some(bind) = &fork.join.bind {
                    defs.insert(bind.name.clone());
                }
            }
            Statement::Loop(looped) => {
                defs.insert(looped.var.clone());
                if let Some(counter) = &looped.counter {
                    defs.insert(counter.name.clone());
                }
            }
            Statement::SubStep(sub) => statement_defs(&sub.body, defs),
            Statement::Spawn(_) | Statement::Sleep(_) | Statement::Route(_) => {}
        }
    }
}
