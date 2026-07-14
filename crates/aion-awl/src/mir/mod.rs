//! The AWL-BC mid-level IR (BC-2). The MIR is the reified catalog of the rev-2
//! Gleam emitter (AWL-BC-IR.md): `lower` consumes the emitter's exact planning
//! passes (D-BC1), `verify` (S1) and `project_sidecar` (S2, D-AOT1) run under
//! every golden, and `print_mir` (§9) is the canonical golden form.
//!
//! Exposed as `#[doc(hidden)] pub` (see `lib.rs`), not a supported API.

mod func;
mod ids;
mod lower;
mod ops;
mod print;
mod runtime;
mod select;
mod shapes;
mod sidecar;
mod tydesc;
mod unit;
mod verify;

#[cfg(test)]
mod child_fork_tests;
#[cfg(test)]
mod codec_tests;
#[cfg(test)]
mod deferred_tests;
#[cfg(test)]
mod fork_tests;
#[cfg(test)]
mod tests;

pub use func::{
    CodecRef, CodecTemplateKind, FlowFn, FnOrigin, FnSig, MirFn, TemplateFn, TrioParams,
    TypeShapeRef,
};
pub use ids::{AtomRef, FnRef, LitRef, Span, Var};
pub use lower::{LowerError, lower};
pub use ops::{Block, BoolBin, CmpOp, JsonVal, LiveAfter, Stmt, Tail, Test, ToJsonRef, Value};
pub use print::print_mir;
pub use runtime::{DurableFamily, RuntimeFn};
pub use select::{SelectError, select};
pub use shapes::{FieldShape, MirLiteral, TypeShape, UnionArm, WireDesc};
pub use sidecar::project_sidecar;
pub use tydesc::{Leaf, TyDesc};
pub use unit::MirModule;
pub use verify::{VerifyError, verify};
