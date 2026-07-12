//! Fork lowering: collection forks ride the SDK `map` (exactly-once per
//! item, input order) or fold sequentially; named-branch forks ride the
//! typed `workflow.all` when every branch calls the same action and the raw
//! wire-unified `workflow.all` otherwise (every branch dispatches as
//! `Activity(String, String)` with byte-identical input, and the join
//! decodes each payload with its action's return codec) — both genuinely
//! parallel, per the spec's "bare `fork` is parallel".

use crate::Spanned;
use crate::ast::{CallStmt, Expr, ForkHeader, ForkStmt, Statement};

use super::context::Emitter;
use super::error::EmitError;
use super::exprs::{Scope, expr_type, render_expr};
use super::names::{ident, string_lit};
use super::stmts::{
    activity_value, activity_value_raw, child_spawn_args, flush_prelude, lower_call,
};
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
            "a collection fork lowers one unbound action or child call per item in the Gleam stopgap",
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
    let returns = if let Some((_, action)) = emitter.actions.get(branch.call.name.as_str()) {
        type_ref_to_g(&action.returns)
    } else if let Some(child) = emitter.children.get(branch.call.name.as_str()) {
        type_ref_to_g(&child.returns)
    } else {
        return Err(EmitError::new(
            branch.call.name_span,
            format!(
                "`{}` names neither a declared action nor a child workflow",
                branch.call.name
            ),
        ));
    };
    let binder = fork
        .join
        .bind
        .as_ref()
        .map_or_else(|| "_".to_owned(), |bind| ident(&bind.name));
    if emitter.children.contains_key(branch.call.name.as_str()) {
        lower_collection_child_fork(
            emitter,
            ChildFork {
                call: branch,
                scope: &branch_scope,
                prelude,
                items: &items,
                var,
                binder: &binder,
                sequential,
            },
        )?;
    } else {
        lower_collection_action_fork(
            emitter,
            ActionFork {
                call: branch,
                scope: &branch_scope,
                prelude,
                items: &items,
                var,
                binder: &binder,
                sequential,
                span: fork.span,
            },
        )?;
    }
    if let Some(bind) = &fork.join.bind {
        scope.insert(bind.name.clone(), GType::List(Box::new(returns)));
    }
    Ok(())
}

struct ChildFork<'a> {
    call: &'a CallStmt,
    scope: &'a Scope,
    prelude: Vec<String>,
    items: &'a str,
    var: &'a str,
    binder: &'a str,
    sequential: bool,
}

fn lower_collection_child_fork(
    emitter: &mut Emitter<'_>,
    fork: ChildFork<'_>,
) -> Result<(), EmitError> {
    if fork.call.config.is_some() {
        return Err(EmitError::new(
            fork.call.span,
            "`node`/`timeout` cannot pin a child workflow call — the engine routes children, not a queue",
        ));
    }
    let child = emitter.children[fork.call.call.name.as_str()];
    let mut branch_prelude = Vec::new();
    let spawn = child_spawn_args(
        emitter,
        child,
        &fork.call.call,
        fork.scope,
        &mut branch_prelude,
    )?;
    emitter.flags.uses_list_module = true;
    flush_prelude(emitter, fork.prelude);
    if fork.sequential {
        emitter.line(&format!(
            "use awl_children_reversed <- result.try(list.try_fold({}, [], fn(awl_acc, {}) {{",
            fork.items,
            ident(fork.var)
        ));
        emitter.indented_try(|this| {
            flush_prelude(this, branch_prelude);
            this.line(&format!(
                "use awl_item <- result.try(workflow.spawn_and_wait{spawn} |> awl_error.map_child_error)"
            ));
            this.line("Ok([awl_item, ..awl_acc])");
            Ok(())
        })?;
        emitter.line("}))");
        emitter.line(&format!(
            "let {} = list.reverse(awl_children_reversed)",
            fork.binder
        ));
        return Ok(());
    }
    if !branch_prelude.is_empty() {
        return Err(EmitError::new(
            fork.call.span,
            "indexing inside a parallel fork branch is not lowerable in the Gleam stopgap",
        ));
    }
    emitter.flags.uses_child_module = true;
    emitter.line(&format!(
        "use awl_handles_reversed <- result.try(list.try_fold({}, [], fn(awl_acc, {}) {{",
        fork.items,
        ident(fork.var)
    ));
    emitter.indented(|this| {
        this.line(&format!(
            "use awl_handle <- result.try(workflow.spawn{spawn} |> awl_error.map_spawn_error)"
        ));
        this.line("Ok([awl_handle, ..awl_acc])");
    });
    emitter.line("}))");
    emitter.line(
        "use awl_children <- result.try(list.try_fold(awl_handles_reversed, [], fn(awl_acc, awl_handle) {",
    );
    emitter.indented(|this| {
        this.line(
            "use awl_item <- result.try(child.await(awl_handle) |> awl_error.map_child_error)",
        );
        this.line("Ok([awl_item, ..awl_acc])");
    });
    emitter.line("}))");
    emitter.line(&format!("let {} = awl_children", fork.binder));
    Ok(())
}

struct ActionFork<'a> {
    call: &'a CallStmt,
    scope: &'a Scope,
    prelude: Vec<String>,
    items: &'a str,
    var: &'a str,
    binder: &'a str,
    sequential: bool,
    span: crate::Span,
}

fn lower_collection_action_fork(
    emitter: &mut Emitter<'_>,
    fork: ActionFork<'_>,
) -> Result<(), EmitError> {
    let mut branch_prelude = Vec::new();
    let value = activity_value(
        emitter,
        &fork.call.call,
        fork.call.config.as_ref(),
        fork.scope,
        &mut branch_prelude,
    )?;
    if fork.sequential {
        emitter.flags.uses_list_module = true;
        flush_prelude(emitter, fork.prelude);
        emitter.line(&format!(
            "use awl_folded <- result.try(list.try_fold({}, [], fn(awl_acc, {}) {{",
            fork.items,
            ident(fork.var)
        ));
        emitter.indented_try(|this| {
            flush_prelude(this, branch_prelude);
            this.line(&format!(
                "use awl_item <- result.try({value} |> workflow.run |> awl_error.map_activity_error)"
            ));
            this.line("Ok([awl_item, ..awl_acc])");
            Ok(())
        })?;
        emitter.line("}))");
        emitter.line(&format!("let {} = list.reverse(awl_folded)", fork.binder));
    } else {
        if !branch_prelude.is_empty() {
            return Err(EmitError::new(
                fork.span,
                "indexing inside a parallel fork branch is not lowerable in the Gleam stopgap",
            ));
        }
        flush_prelude(emitter, fork.prelude);
        emitter.line(&format!(
            "use {} <- result.try(workflow.map({}, fn({}) {{ {value} }}) |> awl_error.map_activity_error)",
            fork.binder,
            fork.items,
            ident(fork.var)
        ));
    }
    Ok(())
}

/// Named-branch fork: the typed `workflow.all` when every branch calls the
/// same action, the raw wire-unified `workflow.all` otherwise. Either way
/// every branch dispatches before any branch is awaited.
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
                let message = if emitter.children.contains_key(call.call.name.as_str()) {
                    "child calls are not yet lowerable inside named fork branches"
                } else {
                    "named fork branches lower as action calls in the Gleam stopgap"
                };
                return Err(EmitError::new(call.span, message));
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
            "use awl_branches <- result.try(workflow.all([{}]) |> awl_error.map_activity_error)",
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
    } else if branches.len() > 1 {
        lower_hetero_parallel(emitter, &branches, scope)?;
    } else {
        for branch in &branches {
            lower_call(emitter, branch, scope)?;
        }
    }
    Ok(())
}

/// Parallel dispatch for differently-typed action calls: the SDK's
/// `workflow.all` is homogeneous in both type parameters, so every call
/// rides its raw wrapper twin (`Activity(String, String)` — the input is
/// pre-encoded with the action's own input codec, so the wire bytes match
/// the typed path exactly) and the join decodes each branch's payload with
/// its action's return codec.
pub(super) fn lower_hetero_parallel(
    emitter: &mut Emitter<'_>,
    calls: &[&CallStmt],
    scope: &mut Scope,
) -> Result<(), EmitError> {
    let mut values = Vec::new();
    for call in calls {
        let mut prelude = Vec::new();
        let value = activity_value_raw(
            emitter,
            &call.call,
            call.config.as_ref(),
            scope,
            &mut prelude,
        )?;
        flush_prelude(emitter, prelude);
        values.push(value);
    }
    emitter.line(&format!(
        "use awl_branches <- result.try(workflow.all([{}]) |> awl_error.map_activity_error)",
        values.join(", ")
    ));
    let patterns = calls
        .iter()
        .enumerate()
        .map(|(position, call)| {
            if call.bind.is_some() {
                format!("awl_raw_{position}")
            } else {
                "_".to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    emitter.line(&format!("let assert [{patterns}] = awl_branches"));
    for (position, call) in calls.iter().enumerate() {
        if let Some(bind) = &call.bind {
            let (_, action) = emitter.actions[call.call.name.as_str()];
            let returns = type_ref_to_g(&action.returns);
            let codec = emitter.codec_fn(&returns);
            emitter.line(&format!(
                "use {} <- result.try(awlc.decoded({codec}(), awl_raw_{position}, {}))",
                ident(&bind.name),
                string_lit(&call.call.name)
            ));
            scope.insert(bind.name.clone(), returns);
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
