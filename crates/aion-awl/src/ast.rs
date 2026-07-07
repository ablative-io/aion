#![allow(missing_docs)]

use crate::{DurationUnit, Span};

#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub span: Span,
    pub workflow: WorkflowDecl,
    pub about: Option<AboutDecl>,
    pub inputs: Vec<IoDecl>,
    pub output: Option<IoDecl>,
    pub error: Option<IoDecl>,
    pub signals: Vec<IoDecl>,
    pub types: Vec<TypeDecl>,
    pub actions: Vec<ActionDecl>,
    pub steps: Vec<StepDecl>,
    pub finish: Expr,
    pub comments: Vec<Comment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub span: Span,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Trivia {
    pub leading: Vec<Comment>,
    pub trailing: Option<Comment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowDecl {
    pub span: Span,
    pub trivia: Trivia,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AboutDecl {
    pub span: Span,
    pub trivia: Trivia,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IoDecl {
    pub span: Span,
    pub trivia: Trivia,
    pub name: String,
    pub ty: TypeRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDecl {
    pub span: Span,
    pub trivia: Trivia,
    pub name: String,
    pub fields: Vec<FieldDecl>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDecl {
    pub span: Span,
    pub name: String,
    pub ty: TypeRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionDecl {
    pub span: Span,
    pub trivia: Trivia,
    pub name: String,
    pub params: Vec<FieldDecl>,
    pub returns: TypeRef,
    pub queue: Option<String>,
    pub node: Option<String>,
    pub timeout: Option<DurationLiteral>,
    pub retry: Option<RetrySpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepDecl {
    pub span: Span,
    pub trivia: Trivia,
    pub name: String,
    pub about: Option<AboutDecl>,
    pub when: Option<Expr>,
    pub each: Option<EachSpec>,
    pub op: StepOp,
    pub repeat: Option<Expr>,
    pub until: Option<Expr>,
    pub retry: Option<RetrySpec>,
    pub timeout: Option<DurationLiteral>,
    pub on_timeout: Option<HandlerBlock>,
    pub on_failure: Option<HandlerBlock>,
    pub bind_as: Option<String>,
    pub queue: Option<String>,
    pub node: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EachSpec {
    pub span: Span,
    pub name: String,
    pub in_expr: Expr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOp {
    Do(CallTarget),
    Wait { span: Span, signal: String },
    Sleep(DurationLiteral),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallTarget {
    Action(CallExpr),
    Child {
        span: Span,
        workflow: String,
        args: Vec<Expr>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallExpr {
    pub span: Span,
    pub name: String,
    pub args: Vec<Expr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerBlock {
    pub span: Span,
    pub actions: Vec<CallTarget>,
    pub terminal: HandlerTerminal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerTerminal {
    Finish(Expr),
    Fail(Span),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetrySpec {
    Every {
        span: Span,
        count: u64,
        every: DurationLiteral,
    },
    Backoff {
        span: Span,
        count: u64,
        min: DurationLiteral,
        max: DurationLiteral,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurationLiteral {
    pub span: Span,
    pub magnitude: u64,
    pub unit: DurationUnit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRef {
    Named { span: Span, name: String },
    List { span: Span, inner: Box<TypeRef> },
    Option { span: Span, inner: Box<TypeRef> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    String {
        span: Span,
        value: String,
    },
    Int {
        span: Span,
        value: u64,
    },
    Float {
        span: Span,
        value: String,
    },
    Bool {
        span: Span,
        value: bool,
    },
    Duration(DurationLiteral),
    List {
        span: Span,
        items: Vec<Expr>,
    },
    Ref {
        span: Span,
        name: String,
    },
    Field {
        span: Span,
        base: Box<Expr>,
        field: String,
    },
    Record {
        span: Span,
        name: String,
        fields: Vec<RecordField>,
    },
    Not {
        span: Span,
        expr: Box<Expr>,
    },
    Binary {
        span: Span,
        left: Box<Expr>,
        op: BinaryOp,
        right: Box<Expr>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordField {
    pub span: Span,
    pub name: String,
    pub value: Expr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
}

pub trait Spanned {
    fn span(&self) -> Span;
}

impl Spanned for Expr {
    fn span(&self) -> Span {
        match self {
            Self::String { span, .. }
            | Self::Int { span, .. }
            | Self::Float { span, .. }
            | Self::Bool { span, .. }
            | Self::List { span, .. }
            | Self::Ref { span, .. }
            | Self::Field { span, .. }
            | Self::Record { span, .. }
            | Self::Not { span, .. }
            | Self::Binary { span, .. } => *span,
            Self::Duration(duration) => duration.span,
        }
    }
}

pub(crate) fn join_span(start: Span, end: Span) -> Span {
    Span {
        start: start.start,
        end: end.end,
        line: start.line,
        column: start.column,
    }
}
