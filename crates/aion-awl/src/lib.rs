//! Front end for the AWL workflow language (rev-2 surface).
//!
//! The `.awl` document is the source of truth; the canonical workflow model
//! is its lossless parse. The lexer is hand-written so the parser can rely
//! on exact token spans, indentation tokens, and AWL's doc-line data tokens
//! (`//!`, `///`); the parser and canonical printer are one property:
//! `parse ∘ print = id`, `print ∘ parse ∘ print = print`.

mod ast;
mod checker;
mod emitter;
mod lexer;
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
    PredicateKind, RetrySpec, RouteDirection, RouteStmt, RouteTarget, SignalDecl, SleepStmt,
    SpawnStmt, Statement, Step, TypeBody, TypeDecl, TypeRef, WaitStmt, WorkerDecl,
};
pub use checker::{CheckError, check, check_in};
pub use emitter::{EmitError, emit, emit_in};
pub use lexer::{DurationUnit, Keyword, LexError, Span, Token, TokenKind, lex};
pub use parser::{ParseError, parse};
pub use printer::print;
pub use schema::{
    SchemaError, schema_for_type, schema_for_type_in, schema_for_workflow, schema_for_workflow_in,
};
pub use spanned::Spanned;
