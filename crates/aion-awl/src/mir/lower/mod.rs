//! `lower`: `CheckedDocument` (rev-2 `Document` + shared planning passes) -> MIR.

mod activity;
mod build;
mod chain;
mod codec;
mod ctx;
mod driver;
mod expr;
mod flow;
mod fork_named;
mod forks;
mod liveness;
mod loops;
mod outcome;
mod slots;
mod wrappers;

pub use driver::{LowerError, lower};
