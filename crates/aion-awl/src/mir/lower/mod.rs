//! `lower`: `CheckedDocument` (rev-2 `Document` + shared planning passes) -> MIR.

mod activity;
mod build;
mod codec;
mod ctx;
mod driver;
mod expr;
mod flow;

pub use driver::{LowerError, lower};
