//! `lower`: `CheckedDocument` (rev-2 `Document` + shared planning passes) -> MIR.

mod activity;
mod build;
mod chain;
mod codec;
mod codec_decode;
mod codec_decode_union;
mod codec_encode;
mod collection_predicate;
mod ctx;
mod driver;
mod expr;
mod flow;
mod fork_action;
mod fork_child;
mod fork_named;
mod forks;
mod liveness;
mod loops;
mod outcome;
mod registry;
mod slots;
mod wrappers;

pub use driver::{LowerError, lower};
