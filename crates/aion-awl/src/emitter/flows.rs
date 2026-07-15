//! Nested-flow emission: subflow function sets, per-item region instance
//! function sets, and the fan-out at each collapsed region step.
//!
//! Delivery shapes (rev 3 §1–§2, B4 memo §3):
//! - parallel `distribute` over a single-activity track: `workflow.map`
//!   (strict) or the settled combinator `workflow.map_settled` with a
//!   per-slot `Option` substitution (tolerant `collect ?`);
//! - `distribute` over a single child-workflow track: spawn-all then
//!   await-each in item order (strict), per-handle `Option` capture
//!   (tolerant);
//! - `sequence`, and any multi-step track: one instance at a time through
//!   the region's generated instance function (`list.try_fold` strict,
//!   `list.fold` with per-instance `Option` capture tolerant). A parallel
//!   `distribute` over a multi-step track degrades to written order with a
//!   generated visibility comment (the SDK parallelizes activities, not
//!   flows) — a recorded mapping limit.

use crate::Spanned;
use crate::ast::{CallStmt, DeliveryVerb, DistributeStmt, Statement};

use super::context::Emitter;
use super::error::EmitError;
use super::exprs::{Scope, expr_type, render_expr};
use super::flowshape::RegionShape;
use super::graph::{NestedPlan, Plans};
use super::names::{ident, snake};
use super::steps::{ExitKind, FlowCtx, FlowExit, annotated_params, emit_flow_fns};
use super::stmts::{activity_value, child_spawn_args, flush_prelude, subflow_fn};
use super::types::{GType, type_ref_to_g};

/// The generated name of a region's per-instance entry wrapper.
fn region_fn(region: &RegionShape) -> String {
    format!("awl_r{}_{}", region.id, snake(&region.open_name))
}

/// Emit every nested flow: each subflow once, and each region's per-item
/// member flow, as a run-once entry wrapper plus its region functions.
pub(super) fn emit_nested(emitter: &mut Emitter<'_>, plans: &Plans<'_>) -> Result<(), EmitError> {
    let shapes = emitter.subflow_shapes;
    for shape in shapes {
        let Some(nested) = plans.subflows.get(&shape.name) else {
            return Err(EmitError::new(
                shape.span,
                format!("subflow `{}` was never planned", shape.name),
            ));
        };
        let ty = type_ref_to_g(&shape.outcome_ty);
        let output = emitter.env.gleam_type(&ty);
        let flow = FlowCtx {
            steps: &shape.flow.steps,
            regions: &shape.flow.regions,
            plan: &nested.plan,
            plans,
            bindings: emitter
                .subflow_bindings
                .get(&shape.name)
                .cloned()
                .unwrap_or_default(),
            prefix: format!("awl_sf_{}_", snake(&shape.name)),
            exit: Some(FlowExit {
                name: shape.outcome_name.clone(),
                kind: ExitKind::Subflow { ty },
            }),
            output,
        };
        emit_wrapper(emitter, &flow, &subflow_fn(&shape.name), nested)?;
        emit_flow_fns(emitter, &flow)?;
    }
    for (&id, nested) in &plans.regions {
        let Some(&region) = plans.region_shapes.get(&id) else {
            return Err(EmitError::new(
                emitter.document.span,
                format!("region {id} lost its shape"),
            ));
        };
        let bindings = emitter
            .region_bindings
            .get(&id)
            .cloned()
            .unwrap_or_default();
        let Some(item_ty) = bindings.get(&region.binding).cloned() else {
            return Err(EmitError::new(
                region.span,
                format!(
                    "the collected binding `{}` has no established type — the document did \
                     not check cleanly",
                    region.binding
                ),
            ));
        };
        let output = emitter.env.gleam_type(&item_ty);
        let flow = FlowCtx {
            steps: &region.members.steps,
            regions: &region.members.regions,
            plan: &nested.plan,
            plans,
            bindings,
            prefix: format!("{}_", region_fn(region)),
            exit: Some(FlowExit {
                name: region.exit_name.clone(),
                kind: ExitKind::Region {
                    binding: region.binding.clone(),
                },
            }),
            output,
        };
        emit_wrapper(emitter, &flow, &region_fn(region), nested)?;
        emit_flow_fns(emitter, &flow)?;
    }
    Ok(())
}

/// One nested flow's run-once entry wrapper: seed the flow's visit counters
/// (so a backward route can never reset a bound) and call the entry step.
fn emit_wrapper(
    emitter: &mut Emitter<'_>,
    flow: &FlowCtx<'_>,
    name: &str,
    nested: &NestedPlan,
) -> Result<(), EmitError> {
    let Some(entry_step) = flow.steps.first() else {
        return Err(EmitError::new(
            emitter.document.span,
            format!("nested flow `{name}` has no steps"),
        ));
    };
    let scope =
        super::steps::scope_from_params(&flow.bindings, &nested.wrapper_params, entry_step)?;
    let rendered_params = annotated_params(emitter, &nested.wrapper_params, &scope);
    let output = flow.output.clone();
    emitter.line(&format!(
        "fn {name}({rendered_params}) -> Result({output}, awl_error.AwlError) {{"
    ));
    let entry_name = entry_step.name.clone();
    emitter.indented(|this| {
        for counter in &nested.counters {
            this.line(&format!("let {} = 0", ident(counter)));
        }
        let args = nested
            .entry_args
            .iter()
            .map(|arg| ident(arg))
            .collect::<Vec<_>>()
            .join(", ");
        this.line(&format!("{}({args})", flow.step_fn(&entry_name)));
    });
    emitter.line("}");
    emitter.blank();
    Ok(())
}

/// The one bare call of a single-step, single-statement member track whose
/// binding is the collected one — the shape that fans out directly through
/// the SDK combinators instead of an instance function.
fn single_member_call(region: &RegionShape) -> Option<&CallStmt> {
    let [step] = region.members.steps.as_slice() else {
        return None;
    };
    if !step.outcomes.is_empty()
        || step.on_failure.is_some()
        || step.max_visits.is_some()
        || !step.after.is_empty()
        || !region.members.regions.is_empty()
    {
        return None;
    }
    match step.body.as_slice() {
        [Statement::Call(call)]
            if call
                .bind
                .as_ref()
                .is_some_and(|bind| bind.name == region.binding) =>
        {
            Some(call)
        }
        _ => None,
    }
}

/// Emit the fan-out of one collapsed region step: dispatch the per-item
/// track over the collection, gather per the collect contract, and bind the
/// gathered collection.
pub(super) fn emit_fanout(
    emitter: &mut Emitter<'_>,
    flow: &FlowCtx<'_>,
    step_name: &str,
    distribute: &DistributeStmt,
    scope: &mut Scope,
) -> Result<(), EmitError> {
    let Some(region) = flow.regions.get(step_name) else {
        return Err(EmitError::new(
            distribute.span,
            format!("step `{step_name}` lost its region shape"),
        ));
    };
    let Some(nested) = flow.plans.regions.get(&region.id) else {
        return Err(EmitError::new(
            distribute.span,
            format!("step `{step_name}` has no planned member flow"),
        ));
    };
    let Some(item_ty) = flow.bindings.get(&region.binding).cloned() else {
        return Err(EmitError::new(
            region.span,
            format!(
                "the collected binding `{}` has no established type",
                region.binding
            ),
        ));
    };
    let mut prelude = Vec::new();
    let items = render_expr(emitter, &region.collection, scope, &mut prelude)?;
    let elem_ty = match emitter
        .env
        .resolve(&expr_type(emitter, &region.collection, scope)?)
    {
        GType::List(inner) => *inner,
        other => {
            return Err(EmitError::new(
                region.collection.span(),
                format!(
                    "`{}` needs a list, found {}",
                    region.verb.as_word(),
                    emitter.env.gleam_type(&other)
                ),
            ));
        }
    };
    flush_prelude(emitter, prelude);
    let var = ident(&region.var);
    let bind = ident(&region.collect_bind);
    let mut branch_scope = scope.clone();
    branch_scope.insert(region.var.clone(), elem_ty);

    if let Some(call) = single_member_call(region) {
        if emitter.actions.contains_key(call.call.name.as_str()) {
            emit_activity_fanout(emitter, region, call, &branch_scope, &items, &var, &bind)?;
        } else if emitter.children.contains_key(call.call.name.as_str()) {
            emit_child_fanout(emitter, region, call, &branch_scope, &items, &var, &bind)?;
        } else {
            emit_instance_fanout(emitter, region, nested, &items, &var, &bind)?;
        }
    } else {
        emit_instance_fanout(emitter, region, nested, &items, &var, &bind)?;
    }

    let slot = if region.tolerant {
        GType::Option(Box::new(item_ty))
    } else {
        item_ty
    };
    scope.insert(region.collect_bind.clone(), GType::List(Box::new(slot)));
    Ok(())
}

/// Fan out a single-activity track through the SDK combinators.
fn emit_activity_fanout(
    emitter: &mut Emitter<'_>,
    region: &RegionShape,
    call: &CallStmt,
    branch_scope: &Scope,
    items: &str,
    var: &str,
    bind: &str,
) -> Result<(), EmitError> {
    let mut branch_prelude = Vec::new();
    let value = activity_value(
        emitter,
        &call.call,
        call.config.as_ref(),
        branch_scope,
        &mut branch_prelude,
    )?;
    emitter.flags.uses_list_module = true;
    match (region.verb, region.tolerant) {
        (DeliveryVerb::Distribute, false) => {
            if !branch_prelude.is_empty() {
                return Err(EmitError::new(
                    call.span,
                    "indexing inside a parallel per-item track is not lowerable in the \
                     Gleam stopgap",
                ));
            }
            emitter.line(&format!(
                "use {bind} <- result.try(workflow.map({items}, fn({var}) {{ {value} }}) |> \
                 awl_error.map_activity_error)"
            ));
        }
        (DeliveryVerb::Distribute, true) => {
            if !branch_prelude.is_empty() {
                return Err(EmitError::new(
                    call.span,
                    "indexing inside a parallel per-item track is not lowerable in the \
                     Gleam stopgap",
                ));
            }
            emitter.line(&format!(
                "let awl_settled = workflow.map_settled({items}, fn({var}) {{ {value} }})"
            ));
            emitter.line(&format!(
                "let {bind} = list.map(awl_settled, fn(awl_slot) {{ case awl_slot {{ \
                 Ok(awl_item) -> Some(awl_item) Error(_) -> None }} }})"
            ));
        }
        (DeliveryVerb::Sequence, false) => {
            emitter.line(&format!(
                "use awl_folded <- result.try(list.try_fold({items}, [], fn(awl_acc, {var}) {{"
            ));
            emitter.indented_try(|this| {
                flush_prelude(this, branch_prelude);
                this.line(&format!(
                    "use awl_item <- result.try({value} |> workflow.run |> \
                     awl_error.map_activity_error)"
                ));
                this.line("Ok([awl_item, ..awl_acc])");
                Ok(())
            })?;
            emitter.line("}))");
            emitter.line(&format!("let {bind} = list.reverse(awl_folded)"));
        }
        (DeliveryVerb::Sequence, true) => {
            emitter.line(&format!(
                "let awl_folded = list.fold({items}, [], fn(awl_acc, {var}) {{"
            ));
            emitter.indented(|this| {
                flush_prelude(this, branch_prelude);
                this.line(&format!("case {value} |> workflow.run {{"));
                this.indented(|this| {
                    this.line("Ok(awl_item) -> [Some(awl_item), ..awl_acc]");
                    this.line("Error(_) -> [None, ..awl_acc]");
                });
                this.line("}");
            });
            emitter.line("})");
            emitter.line(&format!("let {bind} = list.reverse(awl_folded)"));
        }
    }
    Ok(())
}

/// Fan out a single child-workflow track: spawn all, await each in item
/// order (parallel), or spawn-and-wait per item (`sequence`).
fn emit_child_fanout(
    emitter: &mut Emitter<'_>,
    region: &RegionShape,
    call: &CallStmt,
    branch_scope: &Scope,
    items: &str,
    var: &str,
    bind: &str,
) -> Result<(), EmitError> {
    if call.config.is_some() {
        return Err(EmitError::new(
            call.span,
            "`node`/`timeout` cannot pin a child workflow call — the engine routes children, \
             not a queue",
        ));
    }
    let Some(&child) = emitter.children.get(call.call.name.as_str()) else {
        return Err(EmitError::new(
            call.call.name_span,
            format!("`{}` names no declared child workflow", call.call.name),
        ));
    };
    let mut branch_prelude = Vec::new();
    let spawn = child_spawn_args(
        emitter,
        child,
        &call.call,
        branch_scope,
        &mut branch_prelude,
    )?;
    emitter.flags.uses_list_module = true;
    match (region.verb, region.tolerant) {
        (DeliveryVerb::Sequence, false) => {
            emitter.line(&format!(
                "use awl_children_reversed <- result.try(list.try_fold({items}, [], \
                 fn(awl_acc, {var}) {{"
            ));
            emitter.indented_try(|this| {
                flush_prelude(this, branch_prelude);
                this.line(&format!(
                    "use awl_item <- result.try(workflow.spawn_and_wait{spawn} |> \
                     awl_error.map_child_error)"
                ));
                this.line("Ok([awl_item, ..awl_acc])");
                Ok(())
            })?;
            emitter.line("}))");
            emitter.line(&format!("let {bind} = list.reverse(awl_children_reversed)"));
        }
        (DeliveryVerb::Sequence, true) => {
            emitter.line(&format!(
                "let awl_folded = list.fold({items}, [], fn(awl_acc, {var}) {{"
            ));
            emitter.indented(|this| {
                flush_prelude(this, branch_prelude);
                this.line(&format!("case workflow.spawn_and_wait{spawn} {{"));
                this.indented(|this| {
                    this.line("Ok(awl_item) -> [Some(awl_item), ..awl_acc]");
                    this.line("Error(_) -> [None, ..awl_acc]");
                });
                this.line("}");
            });
            emitter.line("})");
            emitter.line(&format!("let {bind} = list.reverse(awl_folded)"));
        }
        (DeliveryVerb::Distribute, tolerant) => {
            if !branch_prelude.is_empty() {
                return Err(EmitError::new(
                    call.span,
                    "indexing inside a parallel per-item track is not lowerable in the \
                     Gleam stopgap",
                ));
            }
            emitter.flags.uses_child_module = true;
            if tolerant {
                super::child_fanout::emit_tolerant(emitter, &spawn, items, var, bind);
            } else {
                super::child_fanout::emit_strict(emitter, &spawn, items, var, bind);
            }
        }
    }
    Ok(())
}

/// Fan out a multi-step track through its generated instance function, one
/// instance at a time (a parallel `distribute` degrades to written order —
/// recorded in the generated module).
fn emit_instance_fanout(
    emitter: &mut Emitter<'_>,
    region: &RegionShape,
    nested: &NestedPlan,
    items: &str,
    var: &str,
    bind: &str,
) -> Result<(), EmitError> {
    if matches!(region.verb, DeliveryVerb::Distribute) {
        emitter.line("// awl stopgap: this distribute's per-item track is a multi-step flow;");
        emitter.line("// instances run one at a time (the Gleam SDK parallelizes single");
        emitter.line("// activities, not flows).");
    }
    let instance = region_fn(region);
    let args = nested
        .wrapper_params
        .iter()
        .map(|name| ident(name))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = var;
    emitter.flags.uses_list_module = true;
    if region.tolerant {
        emitter.line(&format!(
            "let awl_gathered = list.fold({items}, [], fn(awl_acc, {}) {{",
            ident(&region.var)
        ));
        emitter.indented(|this| {
            this.line(&format!("case {instance}({args}) {{"));
            this.indented(|this| {
                this.line("Ok(awl_item) -> [Some(awl_item), ..awl_acc]");
                this.line("Error(_) -> [None, ..awl_acc]");
            });
            this.line("}");
        });
        emitter.line("})");
        emitter.line(&format!("let {bind} = list.reverse(awl_gathered)"));
    } else {
        emitter.line(&format!(
            "use awl_gathered <- result.try(list.try_fold({items}, [], fn(awl_acc, {}) {{",
            ident(&region.var)
        ));
        emitter.indented_try(|this| {
            this.line(&format!("use awl_item <- result.try({instance}({args}))"));
            this.line("Ok([awl_item, ..awl_acc])");
            Ok(())
        })?;
        emitter.line("}))");
        emitter.line(&format!("let {bind} = list.reverse(awl_gathered)"));
    }
    Ok(())
}
