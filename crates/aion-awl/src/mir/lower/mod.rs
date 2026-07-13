//! `lower`: `CheckedDocument` (rev-2 `Document` + shared planning passes) -> MIR.

mod activity;
mod build;
mod chain;
mod codec;
mod ctx;
mod driver;
mod expr;
mod flow;
mod liveness;
mod outcome;

pub use driver::{LowerError, lower};
