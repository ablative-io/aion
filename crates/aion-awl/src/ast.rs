pub use crate::spanned::Spanned;
use crate::{DurationUnit, Span};
/// Parsed representation of a complete workflow document.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    /// Source span from the workflow declaration through the stored finish expression span.
    pub span: Span,
    /// Required workflow declaration that names the document.
    pub workflow: WorkflowDecl,
    /// Optional prose description supplied by the top-level `about` line.
    pub about: Option<AboutDecl>,
    /// Top-level input declarations in source order.
    pub inputs: Vec<IoDecl>,
    /// Optional top-level output type declaration.
    pub output: Option<IoDecl>,
    /// Optional top-level error type declaration.
    pub error: Option<IoDecl>,
    /// Signal declarations accepted by `wait` steps.
    pub signals: Vec<IoDecl>,
    /// Record type declarations available to expressions and fields.
    pub types: Vec<TypeDecl>,
    /// Action declarations callable from steps and handlers.
    pub actions: Vec<ActionDecl>,
    /// Step declarations that make up the workflow body.
    pub steps: Vec<StepDecl>,
    /// Expression evaluated by the final `finish` declaration.
    pub finish: Expr,
    /// Own-line comments immediately preceding the `finish` declaration.
    pub finish_leading: Vec<Comment>,
    /// Same-line trailing comment on the `finish` declaration.
    pub finish_trailing: Option<Comment>,
    /// Own-line comments after the `finish` declaration, at the end of the
    /// document (there is no following line for them to be "leading" of).
    pub epilogue_comments: Vec<Comment>,
    /// Own-line comments followed by trailing comments retained for diagnostics.
    pub comments: Vec<Comment>,
}
/// Own-line or same-line comment captured from the source document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// Source span covering the comment marker and text.
    pub span: Span,
    /// Comment text with the leading marker and spacing removed.
    pub text: String,
}
/// Leading and trailing comment trivia attached to a declaration or field.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Trivia {
    /// Own-line comments immediately preceding the annotated item.
    pub leading: Vec<Comment>,
    /// Same-line comment that follows the annotated item.
    pub trailing: Option<Comment>,
}
/// Top-level `workflow` declaration that gives the document its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowDecl {
    /// Source span covering the full `workflow` declaration line.
    pub span: Span,
    /// Comments attached to the `workflow` declaration.
    pub trivia: Trivia,
    /// Workflow identifier declared after the `workflow` keyword.
    pub name: String,
}
/// Prose `about` declaration attached to a workflow or step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AboutDecl {
    /// Source span covering the full `about` declaration line.
    pub span: Span,
    /// Comments attached to the `about` declaration.
    pub trivia: Trivia,
    /// Free-form description text after the `about` keyword.
    pub text: String,
}
/// Input, output, error, or signal declaration with an associated type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IoDecl {
    /// Source span covering the full I/O declaration line.
    pub span: Span,
    /// Comments attached to the I/O declaration.
    pub trivia: Trivia,
    /// Declared channel or signal name; empty for anonymous output/error declarations.
    pub name: String,
    /// Type reference declared for the I/O value.
    pub ty: TypeRef,
}
/// Record type declaration containing named fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDecl {
    /// Source span covering the complete `type` declaration.
    pub span: Span,
    /// Comments attached to the `type` declaration.
    pub trivia: Trivia,
    /// Load-bearing prose from contiguous `///` lines above the declaration.
    pub description: Option<String>,
    /// Exact text following each source `///`, joined by newlines for reprinting.
    pub description_source: Option<String>,
    /// Type name introduced by the declaration.
    pub name: String,
    /// Field declarations listed inside the record body.
    pub fields: Vec<FieldDecl>,
}
/// Named field and type pair used by type and action declarations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDecl {
    /// Source span of the declaration line that contains this field.
    pub span: Span,
    /// Ordinary comment trivia attached to this record field or action parameter.
    pub trivia: Trivia,
    /// Load-bearing prose from contiguous `///` lines above a record field.
    pub description: Option<String>,
    /// Exact text following each source `///`, joined by newlines for reprinting.
    pub description_source: Option<String>,
    /// Field or parameter name introduced by the declaration.
    pub name: String,
    /// Type reference assigned to the field or parameter.
    pub ty: TypeRef,
}
/// Identifies which routing field of an `ActionDecl` a run of own-line
/// comments precedes, so the printer can re-emit them in the right place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionFieldTag {
    /// The action's optional `queue` routing field.
    Queue,
    /// The action's optional `node` routing field.
    Node,
    /// The action's optional `timeout` routing field.
    Timeout,
    /// The action's optional `retry` routing field.
    Retry,
}
/// Action declaration describing an external callable operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionDecl {
    /// Source span covering the action declaration header line.
    pub span: Span,
    /// Comments attached to the action declaration header.
    pub trivia: Trivia,
    /// Action identifier used by call expressions.
    pub name: String,
    /// Parameter declarations accepted by the action.
    pub params: Vec<FieldDecl>,
    /// Type reference returned by the action.
    pub returns: TypeRef,
    /// Optional queue name used to route action execution.
    pub queue: Option<String>,
    /// Optional node selector used to route action execution.
    pub node: Option<String>,
    /// Optional maximum duration allowed for the action.
    pub timeout: Option<DurationLiteral>,
    /// Optional retry policy applied to action execution.
    pub retry: Option<RetrySpec>,
    /// Own-line comments preceding a routing field, keyed by field.
    pub leading_comments: Vec<(ActionFieldTag, Vec<Comment>)>,
    /// Same-line trailing comment on a routing field, keyed by field.
    pub trailing_comments: Vec<(ActionFieldTag, Comment)>,
}
/// Step `as` binding declaration that names a step result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindDecl {
    /// Source span covering the `as` binding line.
    pub span: Span,
    /// Comments attached to the binding line.
    pub trivia: Trivia,
    /// Result variable introduced by the binding.
    pub name: String,
}
/// Identifies which step field a run of own-line comments precedes, for
/// fields whose value type has no room of its own for leading trivia (the
/// `about` and `as` fields carry their own `Trivia` instead).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepFieldTag {
    /// The step's optional `when` guard expression.
    When,
    /// The step's optional `each` iteration clause.
    Each,
    /// The step operation field: `do`, `wait`, or `sleep`.
    Op,
    /// The step's optional `repeat` expression.
    Repeat,
    /// The step's optional `until` expression.
    Until,
    /// The step's optional retry policy.
    Retry,
    /// The step's optional timeout duration.
    Timeout,
    /// The step's optional timeout handler block.
    OnTimeout,
    /// The step's optional failure handler block.
    OnFailure,
    /// The step's optional queue override.
    Queue,
    /// The step's optional node override.
    Node,
}
/// Workflow step with guards, operation, handlers, and routing metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepDecl {
    /// Source span covering the complete step declaration.
    pub span: Span,
    /// Comments attached to the step header.
    pub trivia: Trivia,
    /// Step identifier declared after the `step` keyword.
    pub name: String,
    /// Optional prose description attached to the step.
    pub about: Option<AboutDecl>,
    /// Optional guard expression that controls whether the step runs.
    pub when: Option<Expr>,
    /// Optional iteration clause for fan-out over a collection expression.
    pub each: Option<EachSpec>,
    /// Required operation performed by the step.
    pub op: StepOp,
    /// Optional integer expression bounding the maximum number of
    /// repetitions (`repeat up to N`).
    pub repeat: Option<Expr>,
    /// Optional expression deciding when repeated execution stops.
    pub until: Option<Expr>,
    /// Optional retry policy for the step operation.
    pub retry: Option<RetrySpec>,
    /// Optional timeout duration for the step operation.
    pub timeout: Option<DurationLiteral>,
    /// Optional handler to run when the step times out.
    pub on_timeout: Option<HandlerBlock>,
    /// Optional handler to run when the step operation fails.
    pub on_failure: Option<HandlerBlock>,
    /// Optional binding that names the step result.
    pub bind_as: Option<BindDecl>,
    /// Optional queue override for this step.
    pub queue: Option<String>,
    /// Optional node override for this step.
    pub node: Option<String>,
    /// Own-line comments preceding a step field, keyed by field.
    pub leading_comments: Vec<(StepFieldTag, Vec<Comment>)>,
    /// Same-line trailing comment on a step field, keyed by field.
    pub trailing_comments: Vec<(StepFieldTag, Comment)>,
}
/// `each` clause that binds one item from a collection expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EachSpec {
    /// Source span covering the full `each` clause.
    pub span: Span,
    /// Loop variable introduced before the `in` keyword.
    pub name: String,
    /// Collection expression evaluated after the `in` keyword.
    pub in_expr: Expr,
}
/// Operation performed by a workflow step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOp {
    /// Call an action or child workflow target.
    Do(CallTarget),
    /// Wait for a named signal before continuing.
    Wait {
        /// Source span covering the full `wait` operation.
        span: Span,
        /// Signal name awaited by the step.
        signal: String,
    },
    /// Sleep for a fixed duration before continuing.
    Sleep(DurationLiteral),
}
/// Target invoked by a `do` operation or handler action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallTarget {
    /// Call a declared action in the current workflow.
    Action(CallExpr),
    /// Call a named child workflow with argument expressions.
    Child {
        /// Source span covering the child workflow call expression after `child`.
        span: Span,
        /// Child workflow identifier being invoked.
        workflow: String,
        /// Argument expressions passed to the child workflow.
        args: Vec<Expr>,
    },
}
/// Action call expression with evaluated arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallExpr {
    /// Source span covering the full call expression.
    pub span: Span,
    /// Action name referenced by the call.
    pub name: String,
    /// Argument expressions supplied to the action.
    pub args: Vec<Expr>,
}
/// Timeout or failure handler containing calls and one terminal outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerBlock {
    /// Source span covering the complete handler block.
    pub span: Span,
    /// Calls performed before the handler terminal.
    pub actions: Vec<CallTarget>,
    /// Own-line comments preceding each entry of `actions`, index-aligned.
    pub action_leading: Vec<Vec<Comment>>,
    /// Same-line trailing comment on each entry of `actions`, index-aligned.
    pub action_trailing: Vec<Option<Comment>>,
    /// Required terminal outcome for the handler block.
    pub terminal: HandlerTerminal,
    /// Own-line comments preceding the terminal (`finish`/`fail`) line.
    pub terminal_leading: Vec<Comment>,
    /// Same-line trailing comment on the terminal (`finish`/`fail`) line.
    pub terminal_trailing: Option<Comment>,
}
/// Terminal outcome required at the end of a handler block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerTerminal {
    /// Finish the workflow with the supplied expression.
    Finish(Expr),
    /// Fail the workflow at the source span of the `fail` keyword.
    Fail(Span),
}
/// Retry policy that controls repeated attempts for actions or steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetrySpec {
    /// Retry a fixed number of times at a constant interval.
    Every {
        /// Source span covering the complete retry clause.
        span: Span,
        /// Number of retry attempts requested by the clause.
        count: u64,
        /// Constant delay between retry attempts.
        every: DurationLiteral,
    },
    /// Retry a fixed number of times with bounded backoff delays.
    Backoff {
        /// Source span covering the complete retry clause.
        span: Span,
        /// Number of retry attempts requested by the clause.
        count: u64,
        /// Minimum delay used by the backoff policy.
        min: DurationLiteral,
        /// Maximum delay used by the backoff policy.
        max: DurationLiteral,
    },
}
/// Duration literal made from a numeric magnitude and unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurationLiteral {
    /// Source span covering the complete duration literal.
    pub span: Span,
    /// Numeric amount written before the duration unit.
    pub magnitude: u64,
    /// Unit suffix used by the duration literal.
    pub unit: DurationUnit,
}
/// Type expression used by declarations and fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRef {
    /// Reference to a named scalar or record type.
    Named {
        /// Source span covering the named type reference.
        span: Span,
        /// Type identifier referenced by name.
        name: String,
    },
    /// List type whose elements share one inner type.
    List {
        /// Source span covering the `List` type constructor token.
        span: Span,
        /// Element type contained by the list.
        inner: Box<TypeRef>,
    },
    /// Optional type that may be absent at runtime.
    Option {
        /// Source span covering the `Option` type constructor token.
        span: Span,
        /// Value type wrapped by the option.
        inner: Box<TypeRef>,
    },
}
/// Expression tree used in guards, calls, records, and finishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// String literal expression preserving its decoded value.
    String {
        /// Source span covering the full string literal.
        span: Span,
        /// Decoded string value without source quotes.
        value: String,
    },
    /// Integer literal expression.
    Int {
        /// Source span covering the full integer literal.
        span: Span,
        /// Parsed unsigned integer value.
        value: u64,
    },
    /// Float literal expression preserving the source lexeme.
    Float {
        /// Source span covering the full float literal.
        span: Span,
        /// Original float text so printing preserves formatting.
        value: String,
    },
    /// Boolean literal expression.
    Bool {
        /// Source span covering the full boolean literal.
        span: Span,
        /// Parsed boolean value.
        value: bool,
    },
    /// Duration literal expression.
    Duration(DurationLiteral),
    /// List literal expression containing zero or more items.
    List {
        /// Source span from `[` through the last item, or just `[` when empty.
        span: Span,
        /// Item expressions in source order.
        items: Vec<Expr>,
    },
    /// Reference to a previously declared value or binding.
    Ref {
        /// Source span covering the reference identifier.
        span: Span,
        /// Referenced value or binding name.
        name: String,
    },
    /// Field access expression on a base expression.
    Field {
        /// Source span from the base expression span through the selected field name.
        span: Span,
        /// Expression that produces the record value being accessed.
        base: Box<Expr>,
        /// Field name selected from the base expression.
        field: String,
    },
    /// Record construction expression with named fields.
    Record {
        /// Source span from the type name through the last field, or just the type name when empty.
        span: Span,
        /// Record type name being constructed.
        name: String,
        /// Field initializers supplied to the record.
        fields: Vec<RecordField>,
    },
    /// Logical negation expression.
    Not {
        /// Source span of the inner expression being negated.
        span: Span,
        /// Inner expression whose truth value is inverted.
        expr: Box<Expr>,
    },
    /// Binary expression with an infix operator.
    Binary {
        /// Source span from the left operand span through the right operand span.
        span: Span,
        /// Left operand of the binary operator.
        left: Box<Expr>,
        /// Operator applied between the operands.
        op: BinaryOp,
        /// Right operand of the binary operator.
        right: Box<Expr>,
    },
}
/// Named field initializer inside a record expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordField {
    /// Source span from the field name through the stored value expression span.
    pub span: Span,
    /// Record field name being initialized.
    pub name: String,
    /// Expression assigned to the record field.
    pub value: Expr,
}
/// Binary operator supported by expressions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    /// Logical disjunction operator.
    Or,
    /// Logical conjunction operator.
    And,
    /// Equality comparison operator.
    Eq,
    /// Inequality comparison operator.
    Ne,
    /// Less-than comparison operator.
    Lt,
    /// Less-than-or-equal comparison operator.
    Le,
    /// Greater-than comparison operator.
    Gt,
    /// Greater-than-or-equal comparison operator.
    Ge,
    /// String concatenation operator: `+` accepts only `String` operands
    /// and yields `String`.
    Add,
}
pub(crate) fn join_span(start: Span, end: Span) -> Span {
    Span {
        start: start.start,
        end: end.end,
        line: start.line,
        column: start.column,
    }
}
