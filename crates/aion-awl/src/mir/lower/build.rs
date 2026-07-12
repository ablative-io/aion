//! Module assembly: the type-shape registry, the function slot plan, the
//! template shells (T-DEF/T-RUN/T-EXEC/T-ACT/T-SIG), and the export set.

use std::collections::BTreeMap;

use crate::RouteDirection;
use crate::emitter::{FieldDef, GType, NamedDef, RecordDef, snake, type_ref_to_g};

use super::super::func::{
    CodecRef, CodecTemplateKind, FnOrigin, FnSig, MirFn, TemplateFn, TypeShapeRef,
};
use super::super::ids::{FnRef, Span};
use super::super::shapes::{FieldShape, TypeShape, UnionArm};
use super::super::tydesc::TyDesc;
use super::codec;
use super::ctx::Ctx;
use super::driver::LowerError;

/// A codec type in the registry: a shape needing a `_codec/_to_json/_decoder`
/// trio, with its stem and the `TyDesc` its codec speaks.
pub(super) struct CodecType {
    pub(super) stem: String,
    pub(super) tydesc: TyDesc,
    pub(super) shape_index: usize,
    pub(super) kind: CodecTemplateKind,
    pub(super) subject: String,
}

/// Resolved function references for cross-referencing during body build.
pub(super) struct FnPlan {
    pub(super) run: FnRef,
    pub(super) definition: FnRef,
    pub(super) execute: FnRef,
    /// Codec stem to its `_codec`/`_to_json`/`_decoder` refs.
    pub(super) codecs: BTreeMap<String, (FnRef, FnRef, FnRef)>,
    /// Action name to its activity-wrapper function ref.
    pub(super) activities: BTreeMap<String, FnRef>,
    /// Region index to its step function ref.
    pub(super) regions: Vec<FnRef>,
}

/// The assembled module skeleton, ready for region-body filling.
pub(super) struct Skeleton {
    pub(super) types: Vec<TypeShape>,
    pub(super) plan: FnPlan,
    pub(super) functions: Vec<MirFn>,
    pub(super) exports: Vec<FnRef>,
}

impl Ctx<'_> {
    /// Build the type-shape registry and the codec-type list.
    pub(super) fn registry(&mut self) -> (Vec<TypeShape>, Vec<CodecType>) {
        let emitter = self.emitter;
        let mut shapes = Vec::new();
        let mut codecs = Vec::new();

        // 1. declared/projected named records and enums, in emission order.
        for name in &emitter.env.order.clone() {
            match emitter.env.get(name) {
                Some(NamedDef::Record(record)) => {
                    let record = record.clone();
                    self.push_record(&mut shapes, &mut codecs, name, &record);
                }
                Some(NamedDef::Enum(variants)) => {
                    let variants = variants.clone();
                    self.push_enum(&mut shapes, &mut codecs, name, &variants);
                }
                Some(NamedDef::Alias(_)) | None => {}
            }
        }
        // 2. action input records.
        let mut actions: Vec<(String, String)> = emitter
            .action_inputs
            .iter()
            .map(|(action, record)| (action.clone(), record.clone()))
            .collect();
        actions.sort();
        for (action, record_name) in actions {
            let Some(&(_, decl)) = emitter.actions.get(action.as_str()) else {
                continue;
            };
            let record = RecordDef {
                fields: decl
                    .params
                    .iter()
                    .map(|param| FieldDef {
                        awl_name: param.name.clone(),
                        ty: type_ref_to_g(&param.ty),
                    })
                    .collect(),
            };
            self.push_record(&mut shapes, &mut codecs, &record_name, &record);
        }
        // 3. the workflow input record.
        let input_name = emitter.input_type.clone();
        let input_record = RecordDef {
            fields: emitter
                .document
                .inputs
                .iter()
                .map(|input| FieldDef {
                    awl_name: input.name.clone(),
                    ty: type_ref_to_g(&input.ty),
                })
                .collect(),
        };
        self.push_record(&mut shapes, &mut codecs, &input_name, &input_record);
        // 4. the outcome union.
        if let Some(union_name) = emitter.union_type.clone() {
            self.push_union(&mut shapes, &mut codecs, &union_name);
        }
        (shapes, codecs)
    }

    fn push_record(
        &mut self,
        shapes: &mut Vec<TypeShape>,
        codecs: &mut Vec<CodecType>,
        name: &str,
        record: &RecordDef,
    ) {
        let tag = self.atom(&snake(name));
        let fields: Vec<FieldShape> = record
            .fields
            .iter()
            .map(|field| FieldShape {
                awl_name: field.awl_name.clone(),
                desc: self.wiredesc(&field.ty),
                optional: matches!(self.emitter.env.resolve(&field.ty), GType::Option(_)),
            })
            .collect();
        let index = shapes.len();
        shapes.push(TypeShape::Record {
            name: name.to_owned(),
            tag,
            fields,
        });
        codecs.push(CodecType {
            stem: snake(name),
            tydesc: TyDesc::Custom {
                module: self.module_name.clone(),
                name: name.to_owned(),
                params: Vec::new(),
            },
            shape_index: index,
            kind: CodecTemplateKind::RecordTrio,
            subject: name.to_owned(),
        });
    }

    fn push_enum(
        &mut self,
        shapes: &mut Vec<TypeShape>,
        codecs: &mut Vec<CodecType>,
        name: &str,
        variants: &[String],
    ) {
        let variant_atoms = variants
            .iter()
            .map(|variant| (self.atom(&snake(variant)), variant.clone()))
            .collect();
        let index = shapes.len();
        shapes.push(TypeShape::Enum {
            name: name.to_owned(),
            variants: variant_atoms,
        });
        codecs.push(CodecType {
            stem: snake(name),
            tydesc: TyDesc::Custom {
                module: self.module_name.clone(),
                name: name.to_owned(),
                params: Vec::new(),
            },
            shape_index: index,
            kind: CodecTemplateKind::EnumTrio,
            subject: name.to_owned(),
        });
    }

    fn push_union(
        &mut self,
        shapes: &mut Vec<TypeShape>,
        codecs: &mut Vec<CodecType>,
        union_name: &str,
    ) {
        let emitter = self.emitter;
        let mut arms = Vec::new();
        for outcome in &emitter.document.outcomes {
            if !matches!(outcome.direction, RouteDirection::Success) {
                continue;
            }
            let Some(info) = emitter.outcomes.get(outcome.name.as_str()) else {
                continue;
            };
            let Some(constructor) = &info.constructor else {
                continue;
            };
            let ctor = self.atom(&snake(constructor));
            arms.push(UnionArm {
                outcome: outcome.name.clone(),
                ctor,
                payload: self.wiredesc(&info.ty),
            });
        }
        let index = shapes.len();
        shapes.push(TypeShape::Union {
            name: union_name.to_owned(),
            arms,
        });
        codecs.push(CodecType {
            stem: snake(union_name),
            tydesc: TyDesc::Custom {
                module: self.module_name.clone(),
                name: union_name.to_owned(),
                params: Vec::new(),
            },
            shape_index: index,
            kind: CodecTemplateKind::UnionTrio,
            subject: union_name.to_owned(),
        });
    }
}

/// Plan the function slots and emit the shells + codec trios; region bodies are
/// filled by the caller (`flow`).
pub(super) fn skeleton(ctx: &mut Ctx<'_>) -> Result<Skeleton, LowerError> {
    let (types, codec_types) = ctx.registry();
    let emitter = ctx.emitter;

    // Slot plan.
    let mut next = 3u32; // 0=run, 1=definition, 2=execute
    let mut codecs = BTreeMap::new();
    for codec_type in &codec_types {
        let base = next;
        codecs.insert(
            codec_type.stem.clone(),
            (FnRef(base), FnRef(base + 1), FnRef(base + 2)),
        );
        next += 3;
    }
    let mut actions: Vec<String> = emitter.actions.keys().map(|k| (*k).to_owned()).collect();
    actions.sort();
    let mut activities = BTreeMap::new();
    for action in &actions {
        activities.insert(action.clone(), FnRef(next));
        next += 1;
    }
    let mut signals: Vec<String> = emitter.signals.keys().map(|k| (*k).to_owned()).collect();
    signals.sort();
    // Signal-ref shells occupy one slot each; nothing references them until a
    // `wait` lowers (deferred), so only the slot count matters here.
    next += u32::try_from(signals.len()).unwrap_or(0);
    let mut regions = Vec::new();
    for _ in 0..ctx.plan.regions.len() {
        regions.push(FnRef(next));
        next += 1;
    }

    let plan = FnPlan {
        run: FnRef(0),
        definition: FnRef(1),
        execute: FnRef(2),
        codecs,
        activities,
        regions,
    };

    // Build functions in slot order.
    let mut functions = Vec::new();
    functions.push(run_shell(ctx, &plan));
    functions.push(definition_shell(ctx, &plan));
    functions.push(execute_shell(ctx, &plan)?);
    for codec_type in &codec_types {
        codec::trio(ctx, &plan, &types, codec_type, &mut functions);
    }
    for action in &actions {
        let shell = activity_shell(ctx, &plan, &types, action);
        functions.push(shell);
    }
    for signal in &signals {
        let shell = signal_shell(ctx, &plan, signal);
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

fn input_codec_ref(plan: &FnPlan, ctx: &Ctx<'_>) -> FnRef {
    let stem = snake(&ctx.emitter.input_type);
    plan.codecs.get(&stem).map_or(plan.run, |trio| trio.0)
}

fn output_codec_ref(plan: &FnPlan, ctx: &Ctx<'_>) -> CodecRef {
    match &ctx.emitter.union_type {
        Some(union_name) => {
            let stem = snake(union_name);
            plan.codecs
                .get(&stem)
                .map_or(CodecRef::SdkNil, |trio| CodecRef::Local(trio.0))
        }
        None => CodecRef::SdkNil,
    }
}

fn run_shell(ctx: &Ctx<'_>, plan: &FnPlan) -> MirFn {
    let sig = FnSig {
        params: vec![TyDesc::Dynamic],
        ret: TyDesc::Result(Box::new(TyDesc::String), Box::new(TyDesc::AwlError)),
    };
    MirFn::Templated {
        name: "run".to_owned(),
        origin: FnOrigin::Run,
        template: TemplateFn::Run {
            input_codec: input_codec_ref(plan, ctx),
            output_codec: output_codec_ref(plan, ctx),
        },
        sig,
        span: zero_span(),
    }
}

fn definition_shell(ctx: &Ctx<'_>, plan: &FnPlan) -> MirFn {
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
    MirFn::Templated {
        name: "definition".to_owned(),
        origin: FnOrigin::Definition,
        template: TemplateFn::Definition {
            workflow_name: ctx.emitter.document.name.clone(),
            input_codec: input_codec_ref(plan, ctx),
            output_codec: output_codec_ref(plan, ctx),
        },
        sig,
        span: zero_span(),
    }
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

fn activity_shell(ctx: &mut Ctx<'_>, plan: &FnPlan, types: &[TypeShape], action: &str) -> MirFn {
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

fn signal_shell(ctx: &mut Ctx<'_>, plan: &FnPlan, signal: &str) -> MirFn {
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

/// Resolve a wire type to an output-codec reference: SDK leaf, module-local
/// trio, or the SDK nil codec.
pub(super) fn codec_ref_for(ctx: &Ctx<'_>, plan: &FnPlan, ty: &GType) -> CodecRef {
    if let Some(leaf) = ctx.leaf_of(ty) {
        return CodecRef::SdkLeaf(leaf);
    }
    let stem = ctx.codec_stem(ty);
    plan.codecs
        .get(&stem)
        .map_or(CodecRef::SdkNil, |trio| CodecRef::Local(trio.0))
}

fn find_shape_ref(types: &[TypeShape], name: &str) -> TypeShapeRef {
    let index = types
        .iter()
        .position(|shape| shape.name() == name)
        .unwrap_or(0);
    TypeShapeRef(u16::try_from(index).unwrap_or(u16::MAX))
}
