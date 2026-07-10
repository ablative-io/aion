//! Fork lowering: collection forks ride the SDK `map` (exactly-once per
//! item, input order) or fold sequentially; named-branch forks ride the
//! homogeneous `workflow.all` when every branch calls the same action and
//! run in written order otherwise (a recorded mapping limit).

use crate::Spanned;
use crate::ast::{CallStmt, Expr, ForkHeader, ForkStmt, Statement};

use super::context::Emitter;
use super::error::EmitError;
use super::exprs::{Scope, expr_type, render_expr};
use super::names::ident;
use super::stmts::{activity_value, flush_prelude, lower_call};
use super::types::{GType, type_ref_to_g};

/// Lower a fork statement.
pub(super) fn lower_fork(
    emitter: &mut Emitter<'_>,
    fork: &ForkStmt,
    scope: &mut Scope,
) -> Result<(), EmitError> {
    match &fork.header {
        ForkHeader::Collection {
            var,
            collection,
            sequential,
            ..
        } => lower_collection_fork(emitter, fork, var, collection, *sequential, scope),
        ForkHeader::Named => lower_named_fork(emitter, fork, scope),
    }
}

/// Collection fork: `workflow.map` (exactly-once per item, input order) or
/// a `list.try_fold` for the `sequential` form.
fn lower_collection_fork(
    emitter: &mut Emitter<'_>,
    fork: &ForkStmt,
    var: &str,
    collection: &Expr,
    sequential: bool,
    scope: &mut Scope,
) -> Result<(), EmitError> {
    let branch = single_action_branch(&fork.body).ok_or_else(|| {
        EmitError::new(
            fork.span,
            "a collection fork lowers one unbound action call per item in the Gleam stopgap",
        )
    })?;
    let mut prelude = Vec::new();
    let items = render_expr(emitter, collection, scope, &mut prelude)?;
    let elem_ty = match emitter.env.resolve(&expr_type(emitter, collection, scope)?) {
        GType::List(inner) => *inner,
        other => {
            return Err(EmitError::new(
                collection.span(),
                format!(
                    "`fork … in` needs a list, found {}",
                    emitter.env.gleam_type(&other)
                ),
            ));
        }
    };
    let mut branch_scope = scope.clone();
    branch_scope.insert(var.to_owned(), elem_ty);
    let (_, action) = *emitter
        .actions
        .get(branch.call.name.as_str())
        .ok_or_else(|| {
            EmitError::new(
                branch.call.name_span,
                format!("`{}` names no declared action", branch.call.name),
            )
        })?;
    let returns = type_ref_to_g(&action.returns);
    let binder = fork
        .join
        .bind
        .as_ref()
        .map_or_else(|| "_".to_owned(), |bind| ident(&bind.name));
    let mut branch_prelude = Vec::new();
    let value = activity_value(
        emitter,
        &branch.call,
        branch.config.as_ref(),
        &branch_scope,
        &mut branch_prelude,
    )?;
    if sequential {
        emitter.flags.uses_list_module = true;
        flush_prelude(emitter, prelude);
        emitter.line(&format!(
            "use awl_folded <- try(list.try_fold({items}, [], fn(awl_acc, {}) {{",
            ident(var)
        ));
        emitter.indented_try(|this| {
            flush_prelude(this, branch_prelude);
            this.line(&format!(
                "use awl_item <- try({value} |> workflow.run |> map_activity_error)"
            ));
            this.line("Ok([awl_item, ..awl_acc])");
            Ok(())
        })?;
        emitter.line("}))");
        emitter.line(&format!("let {binder} = list.reverse(awl_folded)"));
    } else {
        if !branch_prelude.is_empty() {
            return Err(EmitError::new(
                fork.span,
                "indexing inside a parallel fork branch is not lowerable in the Gleam stopgap",
            ));
        }
        flush_prelude(emitter, prelude);
        emitter.line(&format!(
            "use {binder} <- try(workflow.map({items}, fn({}) {{ {value} }}) |> \
             map_activity_error)",
            ident(var)
        ));
    }
    if let Some(bind) = &fork.join.bind {
        scope.insert(bind.name.clone(), GType::List(Box::new(returns)));
    }
    Ok(())
}

/// Named-branch fork: `workflow.all` when every branch calls the same action
/// (the SDK's parallel-all is homogeneous); written-order sequential
/// execution otherwise (a recorded mapping limit — results are identical,
/// wall-clock differs).
fn lower_named_fork(
    emitter: &mut Emitter<'_>,
    fork: &ForkStmt,
    scope: &mut Scope,
) -> Result<(), EmitError> {
    let mut branches = Vec::new();
    for statement in &fork.body {
        match statement {
            Statement::Call(call) if emitter.actions.contains_key(call.call.name.as_str()) => {
                branches.push(call);
            }
            Statement::Call(call) => {
                return Err(EmitError::new(
                    call.span,
                    "named fork branches lower as action calls in the Gleam stopgap",
                ));
            }
            _ => {
                return Err(EmitError::new(
                    fork.span,
                    "named fork branches lower as action calls in the Gleam stopgap",
                ));
            }
        }
    }
    let homogeneous = branches.len() > 1
        && branches
            .iter()
            .all(|branch| branch.call.name == branches[0].call.name);
    if homogeneous {
        let mut values = Vec::new();
        for branch in &branches {
            let mut prelude = Vec::new();
            let value = activity_value(
                emitter,
                &branch.call,
                branch.config.as_ref(),
                scope,
                &mut prelude,
            )?;
            flush_prelude(emitter, prelude);
            values.push(value);
        }
        emitter.line(&format!(
            "use awl_branches <- try(workflow.all([{}]) |> map_activity_error)",
            values.join(", ")
        ));
        let patterns = branches
            .iter()
            .map(|branch| {
                branch
                    .bind
                    .as_ref()
                    .map_or_else(|| "_".to_owned(), |bind| ident(&bind.name))
            })
            .collect::<Vec<_>>()
            .join(", ");
        emitter.line(&format!("let assert [{patterns}] = awl_branches"));
        for branch in &branches {
            if let Some(bind) = &branch.bind {
                let (_, action) = emitter.actions[branch.call.name.as_str()];
                scope.insert(bind.name.clone(), type_ref_to_g(&action.returns));
            }
        }
    } else {
        for branch in &branches {
            lower_call(emitter, branch, scope)?;
        }
    }
    Ok(())
}

fn single_action_branch(body: &[Statement]) -> Option<&CallStmt> {
    match body {
        [Statement::Call(call)] if call.bind.is_none() => Some(call),
        _ => None,
    }
}
