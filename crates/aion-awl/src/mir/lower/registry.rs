//! The type-shape/codec registry (split from `build` for the 500-line law):
//! declared/projected named records and enums, action input records, the
//! workflow input record, the outcome union, and every wire-reachable
//! composite (list/option) shape in stem order — the reference emitter's
//! exact discovery (`emitter/codecs.rs::emit_codecs`,
//! `emitter/composites.rs::composite_codecs`) — followed by strict parent-side
//! child outcome-envelope codecs in payload-stem order.

use std::collections::BTreeMap;

use crate::RouteDirection;
use crate::emitter::{FieldDef, GType, NamedDef, RecordDef, snake, type_ref_to_g};

use super::super::func::CodecTemplateKind;
use super::super::shapes::{FieldShape, TypeShape, UnionArm};
use super::super::tydesc::TyDesc;
use super::build::{CodecPayload, CodecType};
use super::ctx::Ctx;

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
        // 5. composite (list/option) trios for every wire-reachable shape,
        // stem-ordered — the reference's exact discovery
        // (`emitter/composites.rs::composite_codecs`).
        let mut composites: BTreeMap<String, GType> = BTreeMap::new();
        for root in self.composite_roots() {
            self.collect_composites(&root, &mut composites);
        }
        for (stem, ty) in composites {
            codecs.push(CodecType {
                stem,
                tydesc: self.tydesc(&ty),
                kind: CodecTemplateKind::CompositeTrio,
                subject: self.emitter.env.gleam_type(&ty),
                payload: CodecPayload::Composite(self.wiredesc(&ty)),
            });
        }
        // 6. one parent-side child outcome-envelope codec per distinct
        // declared payload type, matching `emitter/codecs.rs`.
        let child_outputs: BTreeMap<String, GType> = emitter
            .document
            .children
            .iter()
            .map(|child| {
                let ty = type_ref_to_g(&child.returns);
                (emitter.env.codec_name(&ty), ty)
            })
            .collect();
        for (payload_stem, ty) in child_outputs {
            codecs.push(CodecType {
                stem: format!("awl_child_output_{payload_stem}"),
                tydesc: self.tydesc(&ty),
                kind: CodecTemplateKind::ChildEnvelopeTrio,
                subject: emitter.env.gleam_type(&ty),
                payload: CodecPayload::ChildEnvelope(self.wiredesc(&ty)),
            });
        }
        (shapes, codecs)
    }

    /// Every wire position a composite shape can be reached from: workflow
    /// inputs, outcome payloads, signal payloads, action params/returns,
    /// child params/returns, and declared/projected record fields
    /// (`emitter/composites.rs:15-48`).
    fn composite_roots(&self) -> Vec<GType> {
        let emitter = self.emitter;
        let mut roots: Vec<GType> = Vec::new();
        for input in &emitter.document.inputs {
            roots.push(type_ref_to_g(&input.ty));
        }
        for outcome in &emitter.document.outcomes {
            roots.push(type_ref_to_g(&outcome.ty));
        }
        for signal in &emitter.document.signals {
            roots.push(type_ref_to_g(&signal.ty));
        }
        for worker in &emitter.document.workers {
            for action in &worker.actions {
                for param in &action.params {
                    roots.push(type_ref_to_g(&param.ty));
                }
                roots.push(type_ref_to_g(&action.returns));
            }
        }
        for child in &emitter.document.children {
            for param in &child.params {
                roots.push(type_ref_to_g(&param.ty));
            }
            roots.push(type_ref_to_g(&child.returns));
        }
        for name in &emitter.env.order {
            if let Some(NamedDef::Record(record)) = emitter.env.get(name) {
                for field in &record.fields {
                    roots.push(field.ty.clone());
                }
            }
        }
        roots
    }

    fn collect_composites(&self, ty: &GType, out: &mut BTreeMap<String, GType>) {
        match ty {
            GType::List(inner) | GType::Option(inner) => {
                self.collect_composites(inner, out);
                out.entry(self.emitter.env.codec_name(ty))
                    .or_insert_with(|| ty.clone());
            }
            GType::Named(name) => {
                if let Some(NamedDef::Alias(inner)) = self.emitter.env.get(name) {
                    let inner = inner.clone();
                    self.collect_composites(&inner, out);
                }
            }
            _ => {}
        }
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
            kind: CodecTemplateKind::RecordTrio,
            subject: name.to_owned(),
            payload: CodecPayload::Shape(index),
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
            kind: CodecTemplateKind::EnumTrio,
            subject: name.to_owned(),
            payload: CodecPayload::Shape(index),
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
            kind: CodecTemplateKind::UnionTrio,
            subject: union_name.to_owned(),
            payload: CodecPayload::Shape(index),
        });
    }
}
