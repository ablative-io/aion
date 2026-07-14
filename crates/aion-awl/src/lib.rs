//! Front end for the AWL workflow language (rev-2 surface).
//!
//! The `.awl` document is the source of truth; the canonical workflow model
//! is its lossless parse. The lexer is hand-written so the parser can rely
//! on exact token spans, indentation tokens, and AWL's doc-line data tokens
//! (`//!`, `///`); the parser and canonical printer are one property:
//! `parse ∘ print = id`, `print ∘ parse ∘ print = print`.

mod ast;
mod checker;
mod compile;
mod emitter;
mod lexer;
// The AWL-BC bytecode MIR backend (BC-2). Exposed as `#[doc(hidden)] pub` — a
// tooling/test seam, NOT a supported public API — because the ratified type
// set (AWL-BC-IR.md Appendix A) includes the intentionally-unused `ResultTry`
// fallback and a not-yet-complete `lower`, which cannot coexist with a
// `pub(crate)` module under the workspace's `-D warnings` + no-`#[allow]`
// discipline (unconstructed private variants are `dead_code`). See
// `AWL-BC-IR.md` "BC-2 implementation status"; tightening to `pub(crate)` is a
// panel/operator decision once `lower` constructs the full op surface and the
// `ResultTry` marker is resolved.
#[doc(hidden)]
pub mod mir;
mod parser;
mod printer;
mod schema;
pub mod semantic;
mod spanned;

pub use ast::{
    ActionDecl, AfterRef, Arg, BinaryOp, Binding, Call, CallStmt, ChildDecl, CombinatorCall,
    CombinatorKind, Comment, ConfigLine, ConfigValue, DocLine, Document, DurationLiteral,
    EnumVariant, Expr, FieldDecl, ForkHeader, ForkStmt, Guard, InputDecl, JoinLine, Lead, LoopStmt,
    LoopTail, OnFailure, OutcomeClause, OutcomeDecl, ParamDecl, PipeEnd, PipeStage, PipeStmt,
    PredicateKind, Quantifier, RetrySpec, RouteDirection, RouteStmt, RouteTarget, SignalDecl,
    SleepStmt, SpawnStmt, Statement, Step, TypeBody, TypeDecl, TypeRef, WaitStmt, WorkerDecl,
    WorkflowTimeoutDecl,
};
pub use checker::{CheckError, check, check_in};
pub use compile::{
    ActionRequirement, CompileError, CompiledWorkflow, action_requirements, compile,
};
pub use emitter::{EmitError, emit, emit_in};
pub use lexer::{DurationUnit, Keyword, LexError, Span, Token, TokenKind, lex};
pub use parser::{ParseError, parse};
pub use printer::print;
pub use schema::{
    SchemaError, schema_for_outcomes, schema_for_outcomes_in, schema_for_type, schema_for_type_in,
    schema_for_workflow, schema_for_workflow_in,
};
pub use spanned::Spanned;
