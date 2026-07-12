//! Canonical MIR golden printer (AWL-BC-IR.md §9).
//!
//! Split for the ≤500-line conventions bar: `module` renders the module-level
//! sections (exports, effect schedule S11, capability summary S16, atoms, type
//! registry) and drives `function`, which renders per-function bodies with
//! `live_after` (S14), `degraded_parallel` (S13), and `CodecTemplate`
//! provenance (S8); `util` holds the shared leaf renderers. Deterministic: no
//! timestamps, no paths beyond the source file name.

mod function;
mod module;
mod util;

pub use module::print_mir;
