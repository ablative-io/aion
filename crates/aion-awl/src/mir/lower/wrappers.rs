//! Wrapper/signal template-shell construction (split from `build` for the
//! 500-line law): the typed activity wrapper (T-ACT), its raw twin
//! (T-ACTRAW, heterogeneous named forks), and the signal-ref shell (T-SIG).

use crate::emitter::{snake, type_ref_to_g};

use super::super::func::{FnOrigin, FnSig, MirFn, TemplateFn, TypeShapeRef};
use super::super::ids::Span;
use super::super::shapes::TypeShape;
use super::super::tydesc::TyDesc;
use super::build::{FnPlan, codec_ref_for};
use super::ctx::Ctx;

fn zero_span() -> Span {
    Span::zero()
}

pub(super) fn activity_shell(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    types: &[TypeShape],
    action: &str,
) -> MirFn {
    let (_, decl) = ctx.emitter.actions[action];
    let params: Vec<TyDesc> = decl
        .params
        .iter()
        .map(|param| ctx.tydesc(&type_ref_to_g(&param.ty)))
        .collect();
    let return_g = type_ref_to_g(&decl.returns);
    let input_name = ctx.emitter.action_inputs[action].clone();
    let input_tydesc = TyDesc::Custom {
        module: ctx.module_name.clone(),
        name: input_name.clone(),
        params: Vec::new(),
    };
    let ret = TyDesc::Activity(Box::new(input_tydesc), Box::new(ctx.tydesc(&return_g)));
    let input_codec = plan
        .codecs
        .get(&snake(&input_name))
        .map_or(plan.run, |trio| trio.0);
    let return_codec = codec_ref_for(ctx, plan, &return_g);
    let shape = find_shape_ref(types, &input_name);
    MirFn::Templated {
        name: format!("{}_activity", snake(action)),
        origin: FnOrigin::ActivityWrapper {
            action: action.to_owned(),
            raw: false,
        },
        template: TemplateFn::ActivityWrapper {
            action: action.to_owned(),
            input: shape,
            params: params.clone(),
            input_codec,
            return_codec,
        },
        sig: FnSig { params, ret },
        span: zero_span(),
    }
}

/// The raw wrapper twin (T-ACTRAW): the same action name and wire bytes (the
/// input record is encoded with the action's own input codec), typed
/// `Activity(String, String)` so heterogeneous named-fork branches share one
/// `workflow.all` list (`emitter/wrappers.rs::raw_wrapper`).
pub(super) fn raw_activity_shell(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    types: &[TypeShape],
    action: &str,
) -> MirFn {
    let (_, decl) = ctx.emitter.actions[action];
    let params: Vec<TyDesc> = decl
        .params
        .iter()
        .map(|param| ctx.tydesc(&type_ref_to_g(&param.ty)))
        .collect();
    let input_name = ctx.emitter.action_inputs[action].clone();
    let input_codec = plan
        .codecs
        .get(&snake(&input_name))
        .map_or(plan.run, |trio| trio.0);
    let shape = find_shape_ref(types, &input_name);
    let ret = TyDesc::Activity(Box::new(TyDesc::String), Box::new(TyDesc::String));
    MirFn::Templated {
        name: format!("{}_activity_raw", snake(action)),
        origin: FnOrigin::ActivityWrapper {
            action: action.to_owned(),
            raw: true,
        },
        template: TemplateFn::ActivityWrapperRaw {
            action: action.to_owned(),
            input: shape,
            params: params.clone(),
            input_codec,
        },
        sig: FnSig { params, ret },
        span: zero_span(),
    }
}

pub(super) fn signal_shell(ctx: &mut Ctx<'_>, plan: &FnPlan, signal: &str) -> MirFn {
    let payload_g = type_ref_to_g(&ctx.emitter.signals[signal].ty);
    let payload_tydesc = ctx.tydesc(&payload_g);
    let payload_codec = codec_ref_for(ctx, plan, &payload_g);
    MirFn::Templated {
        name: format!("{}_signal", snake(signal)),
        origin: FnOrigin::SignalRef {
            signal: signal.to_owned(),
        },
        template: TemplateFn::SignalRef {
            signal: signal.to_owned(),
            payload_codec,
        },
        sig: FnSig {
            params: Vec::new(),
            ret: TyDesc::SignalRef(Box::new(payload_tydesc)),
        },
        span: zero_span(),
    }
}

fn find_shape_ref(types: &[TypeShape], name: &str) -> TypeShapeRef {
    let index = types
        .iter()
        .position(|shape| shape.name() == name)
        .unwrap_or(0);
    TypeShapeRef(u16::try_from(index).unwrap_or(u16::MAX))
}
