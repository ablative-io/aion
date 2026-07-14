//! `.gleam_types` sidecar projection (AWL-BC-IR.md §5 / D-AOT1 / IR-19).
//!
//! The sidecar is `project_sidecar(&MirModule)` (S2): a pure fold over the
//! finished function list through a TOTAL `TyDesc -> TypeDescriptor` mapping
//! (X1 rejected — every function gets a row, no erasure). One source, one
//! ratchet, two artifacts. Determinism: canonical order, no timestamps, no
//! paths.

use gleam_types::{GleamTypes, TypeDescriptor};

use super::tydesc::TyDesc;
use super::unit::MirModule;

/// Project the module's `.gleam_types` sidecar bytes (deterministic).
///
/// Coverage: every function in `functions` order (exports first by
/// construction of `lower`, then locals, then lifted closures). Closure rows
/// use physical BEAM arity (S9 / IR-22) because the signature carries declared
/// params + appended captures.
#[must_use]
pub fn project_sidecar(module: &MirModule) -> Vec<u8> {
    let mut types = GleamTypes::new(module.name.clone());
    for function in &module.functions {
        let arity = MirModule::arity(function);
        let params = function.param_tys().iter().map(map_tydesc).collect();
        let ret = map_tydesc(function.ret_ty());
        types.add_function(function.name().to_owned(), arity_u8(arity), params, ret);
    }
    types.serialize()
}

fn arity_u8(arity: u32) -> u8 {
    u8::try_from(arity).unwrap_or(u8::MAX)
}

/// The total `TyDesc` -> `gleam_types::TypeDescriptor` mapping (§5). Named
/// records/enums/unions carry their own module string (set at lowering time).
pub(crate) fn map_tydesc(ty: &TyDesc) -> TypeDescriptor {
    match ty {
        TyDesc::Bool => TypeDescriptor::Bool,
        TyDesc::Int => TypeDescriptor::Int,
        TyDesc::Float => TypeDescriptor::Float,
        TyDesc::String => TypeDescriptor::String,
        TyDesc::Nil => TypeDescriptor::Nil,
        TyDesc::List(inner) => TypeDescriptor::List(Box::new(map_tydesc(inner))),
        // Empty-list provenance: the value IS a list (§5) — keep the list
        // shape rather than the reference's bare `Nil`.
        TyDesc::Unknown => TypeDescriptor::List(Box::new(TypeDescriptor::Nil)),
        TyDesc::Option(inner) => custom("gleam/option", "Option", vec![map_tydesc(inner)]),
        TyDesc::Result(ok, err) => {
            TypeDescriptor::Result(Box::new(map_tydesc(ok)), Box::new(map_tydesc(err)))
        }
        TyDesc::Tuple(elements) => TypeDescriptor::Tuple(elements.iter().map(map_tydesc).collect()),
        TyDesc::Custom {
            module,
            name,
            params,
        } => TypeDescriptor::CustomType {
            module: module.clone(),
            name: name.clone(),
            type_params: params.iter().map(map_tydesc).collect(),
        },
        TyDesc::Fn(args, ret) => TypeDescriptor::Fn(
            args.iter().map(map_tydesc).collect(),
            Box::new(map_tydesc(ret)),
        ),
        TyDesc::Dynamic => custom("gleam/dynamic", "Dynamic", Vec::new()),
        TyDesc::Json => custom("gleam/json", "Json", Vec::new()),
        TyDesc::AwlError => custom("aion/awl/error", "AwlError", Vec::new()),
        TyDesc::Decoder(inner) => {
            custom("gleam/dynamic/decode", "Decoder", vec![map_tydesc(inner)])
        }
        TyDesc::Codec(inner) => custom("aion/codec", "Codec", vec![map_tydesc(inner)]),
        TyDesc::Activity(input, output) => custom(
            "aion/activity",
            "Activity",
            vec![map_tydesc(input), map_tydesc(output)],
        ),
        TyDesc::ChildHandle(output, error) => custom(
            "aion/child",
            "ChildHandle",
            vec![map_tydesc(output), map_tydesc(error)],
        ),
        // The nominal opaque type is `SignalRef` in `aion/signal`
        // (`aion/workflow` only re-exports it as an alias, which Gleam erases);
        // S10/IR-23 pins the spelling the extractor emits, not the alias.
        TyDesc::SignalRef(inner) => custom("aion/signal", "SignalRef", vec![map_tydesc(inner)]),
        // `aion/workflow.WorkflowDefinition` is an alias; the nominal opaque
        // type lives in `aion/workflow/define` and Gleam erases the alias.
        TyDesc::WorkflowDefinition(input, output, error) => custom(
            "aion/workflow/define",
            "WorkflowDefinition",
            vec![map_tydesc(input), map_tydesc(output), map_tydesc(error)],
        ),
        TyDesc::Duration => custom("aion/duration", "Duration", Vec::new()),
    }
}

fn custom(module: &str, name: &str, type_params: Vec<TypeDescriptor>) -> TypeDescriptor {
    TypeDescriptor::CustomType {
        module: module.to_owned(),
        name: name.to_owned(),
        type_params,
    }
}
