use crate::Span;

use super::expr::DurationLiteral;
use super::steps::Step;
use super::trivia::{Comment, DocLine, Lead};

/// Parsed representation of a complete rev-2 workflow document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    /// Span covering the whole source document.
    pub span: Span,
    /// The `//!` narration lines at the top of the document (one or more).
    pub narration: Vec<DocLine>,
    /// Own-line comments between the narration and the `workflow` line.
    pub lead: Vec<Lead>,
    /// Workflow name declared after the `workflow` keyword.
    pub name: String,
    /// Source span of the workflow name.
    pub name_span: Span,
    /// Same-line comment on the `workflow` line.
    pub trailing: Option<Comment>,
    /// Optional document-level workflow timeout.
    pub timeout: Option<WorkflowTimeoutDecl>,
    /// `input` declarations in source order.
    pub inputs: Vec<InputDecl>,
    /// `signal` declarations in source order.
    pub signals: Vec<SignalDecl>,
    /// Workflow `outcome` declarations in source order (at least one).
    pub outcomes: Vec<OutcomeDecl>,
    /// `type` declarations in source order.
    pub types: Vec<TypeDecl>,
    /// `worker` blocks in source order.
    pub workers: Vec<WorkerDecl>,
    /// `child` workflow declarations in source order.
    pub children: Vec<ChildDecl>,
    /// `step` declarations in source order.
    pub steps: Vec<Step>,
    /// Trailing trivia at the very end of the document.
    pub epilogue: Vec<Lead>,
}

/// One `timeout <duration>` declaration in the workflow header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowTimeoutDecl {
    /// Span covering the declaration line.
    pub span: Span,
    /// Leading trivia before the declaration.
    pub lead: Vec<Lead>,
    /// Same-line trailing comment.
    pub trailing: Option<Comment>,
    /// Duration literal, using the shared AWL duration syntax.
    pub duration: DurationLiteral,
    /// Whether the source duration carried a leading minus sign.
    pub negative: bool,
}

/// One `input name: Type` declaration in the workflow header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDecl {
    /// Span covering the declaration line.
    pub span: Span,
    /// Leading trivia before the declaration.
    pub lead: Vec<Lead>,
    /// Same-line trailing comment.
    pub trailing: Option<Comment>,
    /// Input name.
    pub name: String,
    /// Source span of the input name.
    pub name_span: Span,
    /// Declared contract type.
    pub ty: TypeRef,
}

/// One `signal name: Type` declaration in the workflow header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalDecl {
    /// Span covering the declaration line.
    pub span: Span,
    /// Leading trivia before the declaration.
    pub lead: Vec<Lead>,
    /// Same-line trailing comment.
    pub trailing: Option<Comment>,
    /// Signal name.
    pub name: String,
    /// Source span of the signal name.
    pub name_span: Span,
    /// Declared payload type.
    pub ty: TypeRef,
}

/// Engine terminal status a workflow outcome maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteDirection {
    /// `route success` — the run completes.
    Success,
    /// `route failure` — the run fails.
    Failure,
}

/// One `outcome name: type T, route success|failure` header declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutcomeDecl {
    /// Span covering the declaration line.
    pub span: Span,
    /// Leading trivia before the declaration.
    pub lead: Vec<Lead>,
    /// Same-line trailing comment.
    pub trailing: Option<Comment>,
    /// Outcome name.
    pub name: String,
    /// Source span of the outcome name.
    pub name_span: Span,
    /// Payload type the outcome carries.
    pub ty: TypeRef,
    /// Terminal status the outcome maps to.
    pub direction: RouteDirection,
}

/// Type expression used by declarations, fields, and parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRef {
    /// Reference to a named type (builtin or declared).
    Named {
        /// Source span of the type name.
        span: Span,
        /// Referenced type name.
        name: String,
    },
    /// List type `[T]`.
    List {
        /// Source span from `[` through `]`.
        span: Span,
        /// Element type.
        inner: Box<TypeRef>,
    },
    /// Optional type `T?` — the value may be absent (never null).
    Optional {
        /// Source span from the inner type through `?`.
        span: Span,
        /// Wrapped type.
        inner: Box<TypeRef>,
    },
}

/// A `type` declaration through any of the three doors, or an enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDecl {
    /// Span covering the declaration (header line for multi-line bodies).
    pub span: Span,
    /// Leading trivia before the declaration.
    pub lead: Vec<Lead>,
    /// `///` doc lines attached to the declaration.
    pub docs: Vec<DocLine>,
    /// Same-line trailing comment.
    pub trailing: Option<Comment>,
    /// Declared type name.
    pub name: String,
    /// Source span of the type name.
    pub name_span: Span,
    /// The declaration body.
    pub body: TypeBody,
}

/// Body of a `type` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeBody {
    /// Shorthand record: `type T { field: Type, … }`.
    Record {
        /// Field declarations in source order.
        fields: Vec<FieldDecl>,
    },
    /// Payload-less enum: `type T = A | B | C`.
    Enum {
        /// Variants in source order.
        variants: Vec<EnumVariant>,
    },
    /// Inline raw JSON Schema: `type T = schema { … }`, body verbatim
    /// including the enclosing braces.
    SchemaInline {
        /// The raw schema text, byte-for-byte as authored.
        body: String,
        /// Source span covering the braced body.
        body_span: Span,
    },
    /// File import: `type T = schema("path")`.
    SchemaImport {
        /// Imported schema path as written.
        path: String,
        /// Source span of the path literal.
        path_span: Span,
    },
}

/// One field inside a shorthand record type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDecl {
    /// Span covering the field declaration.
    pub span: Span,
    /// Leading trivia before the field (multi-line bodies only).
    pub lead: Vec<Lead>,
    /// `///` doc lines attached to the field.
    pub docs: Vec<DocLine>,
    /// Same-line trailing comment.
    pub trailing: Option<Comment>,
    /// Field name.
    pub name: String,
    /// Source span of the field name.
    pub name_span: Span,
    /// Declared field type.
    pub ty: TypeRef,
}

/// One bare variant of a payload-less enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumVariant {
    /// Source span of the variant name.
    pub span: Span,
    /// Variant name.
    pub name: String,
}

/// A `worker` block: the task queue and the actions required on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerDecl {
    /// Span covering the `worker` header line.
    pub span: Span,
    /// Leading trivia before the block.
    pub lead: Vec<Lead>,
    /// `///` doc lines attached to the block.
    pub docs: Vec<DocLine>,
    /// Same-line trailing comment on the header.
    pub trailing: Option<Comment>,
    /// Worker (task queue) name.
    pub name: String,
    /// Source span of the worker name.
    pub name_span: Span,
    /// Action requirements declared on this worker (one or more).
    pub actions: Vec<ActionDecl>,
}

/// One `action name(params…) -> Type` requirement inside a worker block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionDecl {
    /// Span covering the action header line.
    pub span: Span,
    /// Leading trivia before the declaration.
    pub lead: Vec<Lead>,
    /// `///` doc lines attached to the action.
    pub docs: Vec<DocLine>,
    /// Same-line trailing comment.
    pub trailing: Option<Comment>,
    /// Action name.
    pub name: String,
    /// Source span of the action name.
    pub name_span: Span,
    /// Typed parameters in source order.
    pub params: Vec<ParamDecl>,
    /// Declared result type.
    pub returns: TypeRef,
    /// Optional per-action config line (`node …, timeout …, retry …`).
    pub config: Option<ConfigLine>,
}

/// One typed parameter of an action or child declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamDecl {
    /// Span covering the parameter.
    pub span: Span,
    /// Parameter name.
    pub name: String,
    /// Source span of the parameter name.
    pub name_span: Span,
    /// Declared parameter type.
    pub ty: TypeRef,
}

/// An indented config line: `node <name>, timeout <duration>, retry …`.
///
/// Appears under an action declaration, or under a call statement as a
/// call-site override (the checker rules on which keys may pin where).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLine {
    /// Span covering the config line.
    pub span: Span,
    /// Leading trivia before the config line.
    pub lead: Vec<Lead>,
    /// Same-line trailing comment.
    pub trailing: Option<Comment>,
    /// Optional `node <name>` routing key.
    pub node: Option<ConfigValue>,
    /// Optional `timeout <duration>` key.
    pub timeout: Option<DurationLiteral>,
    /// Optional retry policy.
    pub retry: Option<RetrySpec>,
}

/// A named config value (the `node` selector).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigValue {
    /// Source span of the value.
    pub span: Span,
    /// The configured name.
    pub name: String,
}

/// Retry policy on an action config line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetrySpec {
    /// `retry N every D` — constant interval.
    Every {
        /// Span covering the complete retry clause.
        span: Span,
        /// Number of retry attempts.
        count: u64,
        /// Constant delay between attempts.
        every: DurationLiteral,
    },
    /// `retry N backoff Dmin..Dmax` — bounded backoff.
    Backoff {
        /// Span covering the complete retry clause.
        span: Span,
        /// Number of retry attempts.
        count: u64,
        /// Minimum backoff delay.
        min: DurationLiteral,
        /// Maximum backoff delay.
        max: DurationLiteral,
    },
}

/// A `child name(params…) -> Type` workflow delegation declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildDecl {
    /// Span covering the declaration line.
    pub span: Span,
    /// Leading trivia before the declaration.
    pub lead: Vec<Lead>,
    /// `///` doc lines attached to the declaration.
    pub docs: Vec<DocLine>,
    /// Same-line trailing comment.
    pub trailing: Option<Comment>,
    /// Child workflow name.
    pub name: String,
    /// Source span of the child name.
    pub name_span: Span,
    /// Typed parameters in source order.
    pub params: Vec<ParamDecl>,
    /// Declared result type.
    pub returns: TypeRef,
}
