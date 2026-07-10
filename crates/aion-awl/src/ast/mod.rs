//! The canonical workflow model: the lossless parse of a rev-2 document.

mod document;
mod expr;
mod steps;
mod trivia;

pub use document::{
    ActionDecl, ChildDecl, ConfigLine, ConfigValue, Document, EnumVariant, FieldDecl, InputDecl,
    OutcomeDecl, ParamDecl, RetrySpec, RouteDirection, SignalDecl, TypeBody, TypeDecl, TypeRef,
    WorkerDecl,
};
pub(crate) use expr::join_span;
pub use expr::{Arg, BinaryOp, DurationLiteral, Expr, PredicateKind};
pub use steps::{
    AfterRef, Binding, Call, CallStmt, CombinatorCall, CombinatorKind, ForkHeader, ForkStmt, Guard,
    JoinLine, LoopStmt, LoopTail, OnFailure, OutcomeClause, PipeEnd, PipeStage, PipeStmt,
    RouteStmt, RouteTarget, SleepStmt, SpawnStmt, Statement, Step, WaitStmt,
};
pub(crate) use trivia::doc_text;
pub use trivia::{Comment, DocLine, Lead};
