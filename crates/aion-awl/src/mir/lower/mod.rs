//! `lower`: `CheckedDocument` (rev-2 `Document` + shared planning passes) -> MIR.

mod activity;
mod build;
mod chain;
mod child_call;
mod codec;
mod codec_child;
mod codec_decode;
mod codec_decode_union;
mod codec_encode;
mod collection_predicate;
mod ctx;
mod driver;
mod expr;
mod fanout;
mod fanout_action;
mod fanout_child;
mod flow;
mod fork_action;
mod fork_child;
mod fork_named;
mod forks;
mod liveness;
mod loops;
mod nested;
mod outcome;
mod pipes;
mod plan_slots;
mod registry;
mod route;
mod slots;
mod visits;
mod wait;
mod wrappers;

pub use driver::{LowerError, lower};
