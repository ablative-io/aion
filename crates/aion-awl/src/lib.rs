//! Lexer for the AWL workflow language.
//!
//! The lexer is intentionally hand-written so the parser can rely on exact token
//! spans, indentation tokens, and AWL's `about` prose mode.

mod ast;
mod checker;
mod emitter;
mod lexer;
mod parser;
mod printer;
mod schema;

pub use ast::{
    AboutDecl, ActionDecl, ActionFieldTag, BinaryOp, BindDecl, CallExpr, CallTarget, Comment,
    Document, DurationLiteral, EachSpec, Expr, FieldDecl, HandlerBlock, HandlerTerminal, IoDecl,
    RecordField, RetrySpec, Spanned, StepDecl, StepFieldTag, StepOp, Trivia, TypeDecl, TypeRef,
    WorkflowDecl,
};
pub use checker::{CheckError, check};
pub use emitter::{EmitError, emit};
pub use lexer::{DurationUnit, Keyword, LexError, Span, Token, TokenKind, lex};
pub use parser::{ParseError, parse};
pub use printer::print;
pub use schema::{SchemaError, schema_for_type};
