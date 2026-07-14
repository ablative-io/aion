//! Module assembly: the type-shape registry, the function slot plan, the
//! template shells (T-DEF/T-RUN/T-EXEC/T-ACT/T-SIG), and the export set.

use std::collections::BTreeMap;

use crate::emitter::{GType, snake, type_ref_to_g};

use super::super::func::{CodecRef, CodecTemplateKind, FnOrigin, FnSig, MirFn, TemplateFn};
use super::super::ids::{FnRef, Span};
use super::super::shapes::{TypeShape, WireDesc};
use super::super::tydesc::TyDesc;
use super::codec;
use super::ctx::Ctx;
use super::driver::LowerError;

/// A codec type in the registry: a shape needing a `_codec/_to_json/_decoder`
/// trio, with its stem and the `TyDesc` its codec speaks.
pub(super) struct CodecType {
    pub(super) stem: String,
    pub(super) tydesc: TyDesc,
    pub(super) kind: CodecTemplateKind,
    pub(super) subject: String,
    pub(super) payload: CodecPayload,
}

/// What a codec type's bodies are stamped from: a `TypeShape` registry index
/// (record/enum/union) or a composite wire descriptor (list/option).
pub(super) enum CodecPayload {
    Shape(usize),
    Composite(WireDesc),
}

impl CodecType {
    /// The `TypeShape` this codec speaks, when it is shape-backed.
    pub(super) fn shape<'t>(&self, types: &'t [TypeShape]) -> Option<&'t TypeShape> {
        match &self.payload {
            CodecPayload::Shape(index) => types.get(*index),
            CodecPayload::Composite(_) => None,
        }
    }
}

/// Resolved function references for cross-referencing during body build.
pub(super) struct FnPlan {
    pub(super) run: FnRef,
    pub(super) definition: FnRef,
    pub(super) execute: FnRef,
    /// Codec stem to its `_codec`/`_to_json`/`_decoder` refs.
    pub(super) codecs: BTreeMap<String, (FnRef, FnRef, FnRef)>,
    /// Codec stem to its reserved lifted-function slots (decoder
    /// continuations, optional-field pair fns, the `Some` wrapper, composite
    /// leaf `to_json` items), in the canonical per-kind order
    /// (`codec::lifted_count` documents it).
    pub(super) codec_lifted: BTreeMap<String, Vec<FnRef>>,
    /// Action name to its activity-wrapper function ref.
    pub(super) activities: BTreeMap<String, FnRef>,
    /// Action name to its raw wrapper twin (`Activity(String, String)`, wire
    /// bytes identical) — planned only for actions a heterogeneous named fork
    /// dispatches (`forks::raw_action_inventory`).
    pub(super) raw_activities: BTreeMap<String, FnRef>,
    /// Region index to its entry-step function ref.
    pub(super) regions: Vec<FnRef>,
    /// Region index to the function refs of its chain steps, one per layer in
    /// chain order; `chains[r][0] == regions[r]`.
    pub(super) chains: Vec<Vec<FnRef>>,
    /// One reserved slot per bounded loop, in the exact order lowering
    /// encounters them (regions in plan order, chain steps in layer order,
    /// statements pre-order — `loops::count_loops`). All loop slots follow
    /// every chain slot, so loop bodies append after region lowering.
    pub(super) loops: Vec<FnRef>,
    /// One reserved slot per fork-lifted function (the `workflow.map` branch
    /// body or a sequential fork's fold body), in the exact traversal order
    /// lowering encounters them (`forks::count_fork_fns` — the same
    /// pre-order discipline as `loops`, descending into loop bodies). All
    /// fork slots follow every loop slot.
    pub(super) forks: Vec<FnRef>,
    /// The fixed child witness function passed to string-name child spawns,
    /// present exactly when a reachable child collection fork needs it.
    pub(super) child_witness: Option<FnRef>,
}

/// The assembled module skeleton, ready for region-body filling.
pub(super) struct Skeleton {
    pub(super) types: Vec<TypeShape>,
    pub(super) plan: FnPlan,
    pub(super) functions: Vec<MirFn>,
    pub(super) exports: Vec<FnRef>,
}

/// Reserved codec slots: stem → trio refs, stem → lifted refs. Each codec
/// type reserves `3 + Σ(lifted)` slots: the trio, then its lifted functions
/// (decoder continuations, optional-field pair fns, the `Some` wrapper,
/// composite leaf `to_json` items) in the canonical per-kind order.
type CodecSlots = (
    BTreeMap<String, (FnRef, FnRef, FnRef)>,
    BTreeMap<String, Vec<FnRef>>,
);

/// How a codec type reads in a collision diagnostic: the declared/synthesized
/// type name, or the composite wire shape that generated the stem.
fn codec_owner(codec_type: &CodecType) -> String {
    match codec_type.kind {
        CodecTemplateKind::CompositeTrio => {
            format!("the composite shape `{}`", codec_type.subject)
        }
        CodecTemplateKind::RecordTrio
        | CodecTemplateKind::EnumTrio
        | CodecTemplateKind::UnionTrio => format!("type `{}`", codec_type.subject),
    }
}

fn plan_codec_slots(
    codec_types: &[CodecType],
    types: &[TypeShape],
    next: &mut u32,
    span: crate::Span,
) -> Result<CodecSlots, LowerError> {
    let mut codecs = BTreeMap::new();
    let mut codec_lifted = BTreeMap::new();
    let mut owners: BTreeMap<&str, &CodecType> = BTreeMap::new();
    for codec_type in codec_types {
        // A duplicate stem (`type ListString` vs the `[String]` composite —
        // both stem `list_string`) would silently cross-wire two trios
        // through one map entry while both still stamp positionally: the S1
        // silent-wrong-codec class. Refuse at compile time. The reference
        // emitter renders BOTH `fn <stem>_codec` definitions and fails
        // downstream at `gleam build`; refusing here is the same loud
        // outcome, earlier. Deliberately `LowerError::Message` (a hard
        // failure, not an `Unsupported`/`Planning` refusal): the BC-3 oracle
        // must fail LOUDLY if a corpus fixture ever hits this, never absorb
        // it into the refused bucket.
        if let Some(prior) = owners.insert(codec_type.stem.as_str(), codec_type) {
            return Err(LowerError::new(
                span,
                format!(
                    "codec name collision: {} and {} both generate the `{}` codec; \
                     rename the declared type so generated codec names stay unique",
                    codec_owner(prior),
                    codec_owner(codec_type),
                    codec_type.stem
                ),
            ));
        }
        let base = *next;
        codecs.insert(
            codec_type.stem.clone(),
            (FnRef(base), FnRef(base + 1), FnRef(base + 2)),
        );
        *next += 3;
        let lifted = codec::lifted_count(codec_type, types);
        let refs: Vec<FnRef> = (0..lifted)
            .map(|offset| FnRef(*next + u32::try_from(offset).unwrap_or(u32::MAX)))
            .collect();
        *next += u32::try_from(lifted).unwrap_or(0);
        codec_lifted.insert(codec_type.stem.clone(), refs);
    }
    Ok((codecs, codec_lifted))
}

/// Plan the function slots and emit the shells + codec trios; region bodies are
/// filled by the caller (`flow`).
pub(super) fn skeleton(ctx: &mut Ctx<'_>) -> Result<Skeleton, LowerError> {
    let (types, codec_types) = ctx.registry();
    let emitter = ctx.emitter;

    let mut next = 3u32; // 0=run, 1=definition, 2=execute
    let (codecs, codec_lifted) =
        plan_codec_slots(&codec_types, &types, &mut next, emitter.document.span)?;
    let mut actions: Vec<String> = emitter.actions.keys().map(|k| (*k).to_owned()).collect();
    actions.sort();
    let mut activities = BTreeMap::new();
    for action in &actions {
        activities.insert(action.clone(), FnRef(next));
        next += 1;
    }
    // Raw wrapper twins for heterogeneous named-fork actions, after every
    // typed wrapper, in sorted-name order (deterministic).
    let raw_actions = super::forks::raw_action_inventory(emitter);
    let mut raw_activities = BTreeMap::new();
    for action in &raw_actions {
        raw_activities.insert(action.clone(), FnRef(next));
        next += 1;
    }
    let mut signals: Vec<String> = emitter.signals.keys().map(|k| (*k).to_owned()).collect();
    signals.sort();
    // Signal-ref shells occupy one slot each; nothing references them until a
    // `wait` lowers (deferred), so only the slot count matters here.
    next += u32::try_from(signals.len()).unwrap_or(0);
    let mut regions = Vec::new();
    let mut chains = Vec::new();
    for region in &ctx.plan.regions {
        regions.push(FnRef(next));
        let slots = region.layers.len().max(1);
        let mut chain = Vec::with_capacity(slots);
        for _ in 0..slots {
            chain.push(FnRef(next));
            next += 1;
        }
        chains.push(chain);
    }
    let mut loops = Vec::new();
    for region in &ctx.plan.regions {
        for step_index in region.layers.iter().flatten() {
            let step = &emitter.document.steps[*step_index];
            for _ in 0..super::loops::count_loops(&step.body) {
                loops.push(FnRef(next));
                next += 1;
            }
        }
    }
    let mut forks = Vec::new();
    let mut child_witness_needed = false;
    for region in &ctx.plan.regions {
        for step_index in region.layers.iter().flatten() {
            let step = &emitter.document.steps[*step_index];
            child_witness_needed |= super::forks::needs_child_witness(&step.body, emitter);
            for _ in 0..super::forks::count_fork_fns(&step.body, emitter) {
                forks.push(FnRef(next));
                next += 1;
            }
        }
    }
    // T-DEAD is appended before T-WIT in `driver`; account for its slot when
    // both kinds of fixed helper are present without perturbing prior refs.
    let child_witness = child_witness_needed.then(|| {
        let dead_offset = u32::from(!activities.is_empty());
        FnRef(next + dead_offset)
    });

    let plan = FnPlan {
        run: FnRef(0),
        definition: FnRef(1),
        execute: FnRef(2),
        codecs,
        codec_lifted,
        activities,
        raw_activities,
        regions,
        chains,
        loops,
        forks,
        child_witness,
    };

    // Build functions in slot order.
    let mut functions = Vec::new();
    functions.push(run_shell(ctx, &plan)?);
    functions.push(definition_shell(ctx, &plan)?);
    functions.push(execute_shell(ctx, &plan)?);
    for codec_type in &codec_types {
        codec::trio(ctx, &plan, &types, codec_type, &mut functions)?;
    }
    for action in &actions {
        let shell = super::wrappers::activity_shell(ctx, &plan, &types, action)?;
        functions.push(shell);
    }
    for action in &raw_actions {
        let shell = super::wrappers::raw_activity_shell(ctx, &plan, &types, action)?;
        functions.push(shell);
    }
    for signal in &signals {
        let shell = super::wrappers::signal_shell(ctx, &plan, signal)?;
        functions.push(shell);
    }
    // Region bodies are appended by `flow::regions`.

    let exports = vec![plan.run, plan.definition, plan.execute];
    Ok(Skeleton {
        types,
        plan,
        functions,
        exports,
    })
}

fn zero_span() -> Span {
    Span::zero()
}

pub(super) fn output_tydesc(ctx: &Ctx<'_>) -> TyDesc {
    match &ctx.emitter.union_type {
        Some(name) => TyDesc::Custom {
            module: ctx.module_name.clone(),
            name: name.clone(),
            params: Vec::new(),
        },
        None => TyDesc::Nil,
    }
}

/// The registered trio for a codec stem — never a silent fallback (S1): a
/// stem the registry cannot resolve is a lowering error, because a wrong
/// codec would be validator-clean and runtime-wrong.
pub(super) fn registered_codec(
    plan: &FnPlan,
    ctx: &Ctx<'_>,
    stem: &str,
) -> Result<(FnRef, FnRef, FnRef), LowerError> {
    plan.codecs.get(stem).copied().ok_or_else(|| {
        LowerError::new(
            ctx.emitter.document.span,
            format!("no codec is registered for `{stem}` (registry reachability bug)"),
        )
    })
}

fn input_codec_ref(plan: &FnPlan, ctx: &Ctx<'_>) -> Result<FnRef, LowerError> {
    let stem = snake(&ctx.emitter.input_type);
    Ok(registered_codec(plan, ctx, &stem)?.0)
}

fn output_codec_ref(plan: &FnPlan, ctx: &Ctx<'_>) -> Result<CodecRef, LowerError> {
    match &ctx.emitter.union_type {
        Some(union_name) => {
            let stem = snake(union_name);
            Ok(CodecRef::Local(registered_codec(plan, ctx, &stem)?.0))
        }
        None => Ok(CodecRef::SdkNil),
    }
}

fn run_shell(ctx: &Ctx<'_>, plan: &FnPlan) -> Result<MirFn, LowerError> {
    let sig = FnSig {
        params: vec![TyDesc::Dynamic],
        ret: TyDesc::Result(Box::new(TyDesc::String), Box::new(TyDesc::AwlError)),
    };
    Ok(MirFn::Templated {
        name: "run".to_owned(),
        origin: FnOrigin::Run,
        template: TemplateFn::Run {
            input_codec: input_codec_ref(plan, ctx)?,
            output_codec: output_codec_ref(plan, ctx)?,
        },
        sig,
        span: zero_span(),
    })
}

fn definition_shell(ctx: &Ctx<'_>, plan: &FnPlan) -> Result<MirFn, LowerError> {
    let input = TyDesc::Custom {
        module: ctx.module_name.clone(),
        name: ctx.emitter.input_type.clone(),
        params: Vec::new(),
    };
    let sig = FnSig {
        params: Vec::new(),
        ret: TyDesc::WorkflowDefinition(
            Box::new(input),
            Box::new(output_tydesc(ctx)),
            Box::new(TyDesc::AwlError),
        ),
    };
    Ok(MirFn::Templated {
        name: "definition".to_owned(),
        origin: FnOrigin::Definition,
        template: TemplateFn::Definition {
            workflow_name: ctx.emitter.document.name.clone(),
            input_codec: input_codec_ref(plan, ctx)?,
            output_codec: output_codec_ref(plan, ctx)?,
        },
        sig,
        span: zero_span(),
    })
}

fn execute_shell(ctx: &Ctx<'_>, plan: &FnPlan) -> Result<MirFn, LowerError> {
    let emitter = ctx.emitter;
    let input = TyDesc::Custom {
        module: ctx.module_name.clone(),
        name: emitter.input_type.clone(),
        params: Vec::new(),
    };
    let sig = FnSig {
        params: vec![input],
        ret: TyDesc::Result(Box::new(output_tydesc(ctx)), Box::new(TyDesc::AwlError)),
    };
    let entry_region = ctx
        .plan
        .regions
        .iter()
        .position(|region| region.entry == 0)
        .ok_or_else(|| {
            LowerError::new(emitter.document.span, "the workflow has no start region")
        })?;
    let params = ctx.plan.region_params(entry_region).to_vec();
    let mut entry_args = Vec::new();
    for param in &params {
        let index = emitter
            .document
            .inputs
            .iter()
            .position(|input| &input.name == param)
            .ok_or_else(|| {
                LowerError::new(
                    emitter.document.span,
                    format!("the workflow start needs `{param}`, which is not an input"),
                )
            })?;
        entry_args.push(u16::try_from(index).unwrap_or(u16::MAX));
    }
    let input_fields = emitter
        .document
        .inputs
        .iter()
        .map(|input| (input.name.clone(), ctx.tydesc(&type_ref_to_g(&input.ty))))
        .collect();
    Ok(MirFn::Templated {
        name: "execute".to_owned(),
        origin: FnOrigin::Execute,
        template: TemplateFn::Execute {
            input_fields,
            entry: plan.regions[entry_region],
            entry_args,
        },
        sig,
        span: zero_span(),
    })
}

/// The shared dead-body function (`fn(_) -> Error(error.terminal(...))`, §2.4
/// T-DEAD): a real `module.functions` entry (S8 — `select` never synthesizes a
/// function) that every T-ACT wrapper closes over, so the `FunT` lambda and the
/// `.gleam_types` sidecar both carry it. `lower` appends exactly one per module
/// that has any activity. BC-3 expands the body from `TemplateFn::DeadBody`.
pub(super) fn dead_shell() -> MirFn {
    MirFn::Templated {
        name: "awl$dead_body".to_owned(),
        origin: FnOrigin::DeadBody,
        template: TemplateFn::DeadBody,
        sig: FnSig {
            params: vec![TyDesc::Dynamic],
            ret: TyDesc::Result(Box::new(TyDesc::Dynamic), Box::new(TyDesc::AwlError)),
        },
        span: zero_span(),
    }
}

/// The SDK's string-name spawn witness: a type anchor the engine never calls.
/// BC-3 expands this fixed shell to `Error(AwlChildFailed(message))`.
pub(super) fn child_witness_shell() -> MirFn {
    MirFn::Templated {
        name: "awl$child_witness".to_owned(),
        origin: FnOrigin::ChildWitness,
        template: TemplateFn::ChildWitness,
        sig: FnSig {
            params: vec![TyDesc::Json],
            ret: TyDesc::Result(Box::new(TyDesc::Dynamic), Box::new(TyDesc::AwlError)),
        },
        span: zero_span(),
    }
}

/// Resolve a wire type to an output-codec reference: an SDK leaf or a
/// module-local trio. A non-leaf stem the registry cannot resolve is a
/// lowering error, never a silent `nil_codec` (S1 — the Nil fallback was the
/// wrong-runtime-values masking bug).
pub(super) fn codec_ref_for(
    ctx: &Ctx<'_>,
    plan: &FnPlan,
    ty: &GType,
) -> Result<CodecRef, LowerError> {
    if let Some(leaf) = ctx.leaf_of(ty) {
        return Ok(CodecRef::SdkLeaf(leaf));
    }
    let stem = ctx.codec_stem(ty);
    Ok(CodecRef::Local(registered_codec(plan, ctx, &stem)?.0))
}
