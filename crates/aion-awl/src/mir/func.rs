//! Functions: template shells (BC-3-expanded) and flow functions (bodies from
//! the closed op set), with `FnOrigin` provenance on every one (S6) and the
//! codec-template parameters retained for a post-BC descriptor revisit (S8).

use super::ids::{FnRef, Span};
use super::ops::Block;
use super::shapes::WireDesc;
use super::tydesc::{Leaf, TyDesc};

/// Index into [`crate::mir::MirModule::types`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeShapeRef(pub u16);

/// An output-codec reference: a module-local codec fn, the SDK `nil_codec`
/// (IR-21), or an SDK leaf codec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecRef {
    Local(FnRef),
    SdkNil,
    SdkLeaf(Leaf),
}

/// A template shell: name-substitution only, BC-3 expands each from a fixed
/// recipe (AWL-BC-IR.md §2.4). Post-S8 these are the ONLY templated functions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateFn {
    Definition {
        workflow_name: String,
        input_codec: FnRef,
        output_codec: CodecRef,
    },
    Run {
        input_codec: FnRef,
        output_codec: CodecRef,
    },
    Execute {
        input_fields: Vec<(String, TyDesc)>,
        entry: FnRef,
        entry_args: Vec<u16>,
    },
    ActivityWrapper {
        action: String,
        input: TypeShapeRef,
        params: Vec<TyDesc>,
        input_codec: FnRef,
        return_codec: CodecRef,
    },
    ActivityWrapperRaw {
        action: String,
        input: TypeShapeRef,
        params: Vec<TyDesc>,
        input_codec: FnRef,
    },
    SignalRef {
        signal: String,
        payload_codec: CodecRef,
    },
    DeadBody,
    ChildWitness,
}

/// The codec-template shape a stamped `FlowFn` was produced from (S8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecTemplateKind {
    RecordTrio,
    EnumTrio,
    UnionTrio,
    CompositeTrio,
}

/// Descriptor-style template parameters retained for a post-BC descriptor
/// revisit (S8 / continuation-first D6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrioParams {
    Record {
        shape: TypeShapeRef,
    },
    Enum {
        shape: TypeShapeRef,
    },
    /// The decoder-failure zero is OP-built in the body (S5), never a literal.
    Union {
        shape: TypeShapeRef,
    },
    Composite {
        desc: WireDesc,
    },
}

/// Provenance on every MIR function (S6): richer than name-scheme identity;
/// feeds goldens, sidecar ordering, and BC-3 review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FnOrigin {
    Run,
    Definition,
    Execute,
    Region {
        entry_step: String,
    },
    SubStep {
        parent: String,
        sub: String,
    },
    Loop {
        step: String,
        index: u32,
    },
    ActivityWrapper {
        action: String,
        raw: bool,
    },
    SignalRef {
        signal: String,
    },
    DeadBody,
    ChildWitness,
    CodecTemplate {
        kind: CodecTemplateKind,
        subject: String,
        params: TrioParams,
    },
    LiftedClosure {
        host: FnRef,
        index: u32,
    },
}

/// A flow function: a body drawn from the closed op set, with liveness params
/// and the sidecar-projection signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowFn {
    pub origin: FnOrigin,
    pub name: String,
    /// Declared params (or declared args + appended captures for `Lifted`).
    pub params: Vec<super::ids::Var>,
    /// Physical BEAM arity types for `Lifted` (S9 / IR-22).
    pub param_tys: Vec<TyDesc>,
    pub ret_ty: TyDesc,
    pub body: Block,
    pub span: Span,
    /// S13 marker: a multi-statement dependency-parallel layer lowered in
    /// written order (printed in goldens).
    pub degraded_parallel: bool,
}

/// A function signature: the sidecar-projection source (S2). Held explicitly
/// on template shells so `project_sidecar` is a pure fold over every function
/// with no per-shell special-casing (X1: no omission).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnSig {
    pub params: Vec<TyDesc>,
    pub ret: TyDesc,
}

/// A module function: a template shell (post-S8) or a flow function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirFn {
    Templated {
        name: String,
        origin: FnOrigin,
        template: TemplateFn,
        sig: FnSig,
        span: Span,
    },
    Flow(FlowFn),
}

impl MirFn {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Templated { name, .. } => name,
            Self::Flow(flow) => &flow.name,
        }
    }

    pub(crate) fn origin(&self) -> &FnOrigin {
        match self {
            Self::Templated { origin, .. } => origin,
            Self::Flow(flow) => &flow.origin,
        }
    }

    /// The physical parameter type descriptors (declared params + appended
    /// captures for lifted closures, S9). Length is the physical BEAM arity.
    pub(crate) fn param_tys(&self) -> &[TyDesc] {
        match self {
            Self::Templated { sig, .. } => &sig.params,
            Self::Flow(flow) => &flow.param_tys,
        }
    }

    pub(crate) fn ret_ty(&self) -> &TyDesc {
        match self {
            Self::Templated { sig, .. } => &sig.ret,
            Self::Flow(flow) => &flow.ret_ty,
        }
    }
}
