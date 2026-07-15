mod bindings;
mod codecs;
mod collection_predicates;
mod composites;
mod context;
mod entry;
mod error;
mod exprs;
mod failure;
mod flows;
mod flowshape;
mod forks;
mod frame;
mod graph;
mod liveness;
mod loops;
mod names;
mod outcomes;
mod pipes;
mod project;
mod steps;
mod stmts;
mod subs;
mod types;
mod wrappers;

pub use entry::{emit, emit_in};
pub use error::EmitError;

// Crate-internal planning surface consumed by the AWL-BC MIR backend
// (`crate::mir`). Widening these to `pub(crate)` is the anti-drift lever of
// AWL-BC-IR.md §4: the bytecode `lower` reuses the emitter's exact passes and
// type environment rather than re-deriving them.
pub(crate) use collection_predicates::is_fallible as predicate_is_fallible;
pub(crate) use context::Emitter;
pub(crate) use entry::{prepare, shape_document};
pub(crate) use graph::{Plan, expr_refs};
pub(crate) use loops::{first_route_span, statement_defs, statements_expr_refs};
pub(crate) use names::snake;
pub(crate) use types::{FieldDef, GType, NamedDef, RecordDef, type_ref_to_g};
