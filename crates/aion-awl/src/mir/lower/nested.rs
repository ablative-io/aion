//! Nested-flow lowering: each per-item region member flow lowers once as a
//! run-once entry wrapper (`awl_r<id>_<open>`, the MIR twin of
//! `emitter/flows.rs::emit_wrapper`) plus its own region-chain function set,
//! and each region that runs as an implicit child gains its adapter shells
//! (`<child>_execute` unpacking the input record, and the exported
//! `<child>_run` engine entry — `emitter/implicit_children.rs::emit_adapter`).

use crate::emitter::snake;

use super::super::func::{ExecArg, FlowFn, FnOrigin, FnSig, MirFn, TemplateFn};
use super::super::ids::Span;
use super::super::ops::{Block, Tail, Value};
use super::super::tydesc::TyDesc;
use super::build::{FnPlan, child_output_codec_ref_for, registered_codec};
use super::ctx::Ctx;
use super::driver::LowerError;
use super::flow::{FlowCtx, FlowEnv, FlowExit, lower_flow_fns};
use super::slots::Slots;

/// Lower every region's member flow (ascending region id): the run-once
/// wrapper, then the flow's chain functions, appended in slot order.
pub(super) fn lower_nested_flows(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    functions: &mut Vec<MirFn>,
    slots: &mut Slots,
) -> Result<(), LowerError> {
    let region_ids: Vec<usize> = ctx.plans.regions.keys().copied().collect();
    for id in region_ids {
        lower_region_flow(ctx, plan, id, functions, slots)?;
    }
    Ok(())
}

fn lower_region_flow(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    id: usize,
    functions: &mut Vec<MirFn>,
    slots: &mut Slots,
) -> Result<(), LowerError> {
    let emitter = ctx.emitter;
    let nested = ctx.plans.regions.get(&id).ok_or_else(|| planning(id))?;
    let &shape = ctx
        .plans
        .region_shapes
        .get(&id)
        .ok_or_else(|| planning(id))?;
    let fns = plan.region_fns.get(&id).ok_or_else(|| planning(id))?;
    let bindings = emitter.region_bindings.get(&id).ok_or_else(|| {
        LowerError::new(
            shape.span,
            format!("region {id} has no binding environment"),
        )
    })?;
    let item_ty = bindings.get(&shape.binding).cloned().ok_or_else(|| {
        LowerError::new(
            shape.span,
            format!(
                "the collected binding `{}` has no established type — the document did \
                 not check cleanly",
                shape.binding
            ),
        )
    })?;
    let ret_ty = TyDesc::Result(Box::new(ctx.tydesc(&item_ty)), Box::new(TyDesc::AwlError));
    let flow = FlowCtx {
        steps: &shape.members.steps,
        regions: &shape.members.regions,
        plan: &nested.plan,
        fns: &fns.fns,
        bindings,
        exit: Some(FlowExit {
            name: shape.exit_name.clone(),
            binding: shape.binding.clone(),
        }),
        prefix: format!("awl_r{id}_{}_", snake(&shape.open_name)),
        label: Some(format!("r{id}:{}", shape.open_name)),
        ret_ty: ret_ty.clone(),
    };
    // The wrapper occupies the slot immediately before the flow's chains.
    let wrapper = wrapper_fn(ctx, id, &flow, nested, shape, ret_ty)?;
    functions.push(MirFn::Flow(wrapper));
    let env = FlowEnv { plan, flow: &flow };
    lower_flow_fns(ctx, env, functions, slots)
}

/// The run-once entry wrapper: seed the flow's visit counters (so a backward
/// route can never reset a bound) and tail-call the entry step.
fn wrapper_fn(
    ctx: &mut Ctx<'_>,
    id: usize,
    flow: &FlowCtx<'_>,
    nested: &crate::emitter::NestedPlan,
    shape: &crate::emitter::RegionShape,
    ret_ty: TyDesc,
) -> Result<FlowFn, LowerError> {
    ctx.reset_vars();
    let span = Span::from_source(shape.span);
    let mut params = Vec::new();
    let mut param_tys = Vec::new();
    let mut vars = std::collections::BTreeMap::new();
    for name in &nested.wrapper_params {
        let ty = flow.bindings.get(name).ok_or_else(|| {
            LowerError::new(
                shape.span,
                format!("implicit child parameter `{name}` has no established type"),
            )
        })?;
        let var = ctx.fresh_var();
        params.push(var);
        param_tys.push(ctx.tydesc(ty));
        vars.insert(name.clone(), var);
    }
    let entry_region = flow
        .plan
        .region_of_entry(0)
        .ok_or_else(|| LowerError::new(shape.span, "the member flow has no entry region"))?;
    // Language-owned visit counters seed as literal zero arguments of the
    // entry tail call, so a backward route can never reset a bound.
    let mut args = Vec::new();
    for name in &nested.entry_args {
        match vars.get(name) {
            Some(var) => args.push(Value::Var(*var)),
            None if nested.counters.contains(name) => args.push(Value::Int(0)),
            None => {
                return Err(LowerError::new(
                    shape.span,
                    format!(
                        "the member flow's entry needs `{name}`, which is neither a wrapper \
                         parameter nor a language-owned counter — the document did not check \
                         cleanly"
                    ),
                ));
            }
        }
    }
    Ok(FlowFn {
        origin: FnOrigin::RegionWrapper {
            region: id,
            open: shape.open_name.clone(),
        },
        name: format!("awl_r{id}_{}", snake(&shape.open_name)),
        params,
        param_tys,
        ret_ty,
        body: Block {
            stmts: Vec::new(),
            tail: Tail::TailLocal {
                callee: flow.fns.regions[entry_region],
                args,
            },
        },
        span,
        degraded_parallel: false,
    })
}

/// The adapter shells of every implicit per-item child, ascending region id:
/// `<child>_execute` (a T-EXEC recipe over the child input record, entering
/// the instance wrapper) then the exported `<child>_run` (a T-RUN recipe with
/// the child's own codecs and executor).
pub(super) fn adapter_shells(ctx: &Ctx<'_>, plan: &FnPlan) -> Result<Vec<MirFn>, LowerError> {
    let emitter = ctx.emitter;
    let mut shells = Vec::new();
    for (&id, adapter) in &plan.adapters {
        let nested = ctx.plans.regions.get(&id).ok_or_else(|| planning(id))?;
        let &shape = ctx
            .plans
            .region_shapes
            .get(&id)
            .ok_or_else(|| planning(id))?;
        let fns = plan.region_fns.get(&id).ok_or_else(|| planning(id))?;
        let bindings = emitter.region_bindings.get(&id).ok_or_else(|| {
            LowerError::new(
                shape.span,
                format!("region {id} has no binding environment"),
            )
        })?;
        let item_ty = bindings.get(&shape.binding).cloned().ok_or_else(|| {
            LowerError::new(
                shape.span,
                format!(
                    "the collected binding `{}` has no established type",
                    shape.binding
                ),
            )
        })?;
        let record_name = emitter
            .region_input_types
            .get(&id)
            .cloned()
            .ok_or_else(|| {
                LowerError::new(
                    shape.span,
                    format!("region {id} lost its implicit child input type"),
                )
            })?;
        let mut input_fields = Vec::with_capacity(nested.wrapper_params.len());
        for name in &nested.wrapper_params {
            let ty = bindings.get(name).ok_or_else(|| {
                LowerError::new(
                    shape.span,
                    format!("implicit child parameter `{name}` has no established type"),
                )
            })?;
            input_fields.push((name.clone(), ctx.tydesc(ty)));
        }
        let entry_args = (0..input_fields.len())
            .map(|index| ExecArg::Field(u16::try_from(index).unwrap_or(u16::MAX)))
            .collect();
        let child = snake(&shape.child_name);
        let input_desc = TyDesc::Custom {
            module: ctx.module_name.clone(),
            name: record_name.clone(),
            params: Vec::new(),
        };
        let item_desc = ctx.tydesc(&item_ty);
        let span = Span::from_source(shape.span);
        shells.push(MirFn::Templated {
            name: format!("{child}_execute"),
            origin: FnOrigin::ChildExecute {
                child: shape.child_name.clone(),
            },
            template: TemplateFn::Execute {
                input_fields,
                entry: fns.wrapper,
                entry_args,
            },
            sig: FnSig {
                params: vec![input_desc],
                ret: TyDesc::Result(Box::new(item_desc), Box::new(TyDesc::AwlError)),
            },
            span,
        });
        let input_codec = registered_codec(plan, ctx, &snake(&record_name))?.0;
        let output_codec = child_output_codec_ref_for(ctx, plan, &item_ty)?;
        shells.push(MirFn::Templated {
            name: format!("{child}_run"),
            origin: FnOrigin::ChildRun {
                child: shape.child_name.clone(),
            },
            template: TemplateFn::ChildRun {
                input_codec,
                output_codec,
                execute: adapter.execute,
            },
            sig: FnSig {
                params: vec![TyDesc::Dynamic],
                ret: TyDesc::Result(Box::new(TyDesc::String), Box::new(TyDesc::AwlError)),
            },
            span,
        });
    }
    Ok(shells)
}

fn planning(id: usize) -> LowerError {
    LowerError::Planning {
        message: format!("region {id} was never planned"),
    }
}
