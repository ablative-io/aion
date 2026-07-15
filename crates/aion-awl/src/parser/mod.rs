//! The rev-2 parser: token stream in, canonical workflow model out, with
//! compiler-quality diagnostics on source-correct spans.

mod args;
mod document;
mod error;
mod exprs;
mod flow;
mod hints;
mod statements;
mod steps;
mod stream;
mod types;
mod workers;

pub use document::parse;
pub use error::ParseError;
