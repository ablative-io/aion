use crate::Span;

use super::expr::{Arg, DurationLiteral, Expr};
use super::trivia::{Comment, DocLine, Lead};

/// A `step` declaration: dependencies, body statements, and outcome clauses.
///
/// Steps share the unified anatomy — everything that runs has inputs and
/// outcomes — and may nest: a substep appears as a body statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// Span covering the step header line.
    pub span: Span,
    /// Leading trivia before the step.
    pub lead: Vec<Lead>,
    /// `///` doc lines attached to the step.
    pub docs: Vec<DocLine>,
    /// Same-line trailing comment on the header.
    pub trailing: Option<Comment>,
    /// Step name.
    pub name: String,
    /// Source span of the step name.
    pub name_span: Span,
    /// `after` dependencies (empty means fall-through or route-targeted).
    pub after: Vec<AfterRef>,
    /// Body statements in written order.
    pub body: Vec<Statement>,
    /// Optional `on failure` compensation block.
    pub on_failure: Option<OnFailure>,
    /// Outcome clauses, evaluated in written order after the body.
    pub outcomes: Vec<OutcomeClause>,
}

/// One name in a step's `after` dependency list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AfterRef {
    /// Source span of the dependency name.
    pub span: Span,
    /// Referenced step name.
    pub name: String,
}

/// One statement in a step, fork, loop, or `on failure` body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Statement {
    /// Action or child call, optionally bound and optionally pinned.
    Call(CallStmt),
    /// Fire-and-forget child start: `spawn name(args…)`.
    Spawn(SpawnStmt),
    /// Pipe chain: `head |> stage |> … -> name` or `… |> route target`.
    Pipe(PipeStmt),
    /// Durable signal gate: `wait signal [timeout D] -> name`.
    Wait(WaitStmt),
    /// Durable timer: `sleep D`.
    Sleep(SleepStmt),
    /// Intra-step parallelism: `fork … join`.
    Fork(ForkStmt),
    /// Bounded iteration: `loop … until … max`.
    Loop(LoopStmt),
    /// A `route <target>` line (the terminal of an `on failure` block).
    Route(RouteStmt),
    /// A nested substep.
    SubStep(Box<Step>),
}

/// An action or child call with arguments: `name(arg: expr, …)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// Span from the callee name through the closing parenthesis.
    pub span: Span,
    /// Callee name.
    pub name: String,
    /// Source span of the callee name.
    pub name_span: Span,
    /// Named arguments in source order.
    pub args: Vec<Arg>,
}

/// A `-> name` result binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// Source span of the binding name.
    pub span: Span,
    /// Bound name.
    pub name: String,
}

/// A call statement: `name(args…) [-> binding]` with an optional indented
/// call-site config override line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallStmt {
    /// Span covering the statement line.
    pub span: Span,
    /// Leading trivia before the statement.
    pub lead: Vec<Lead>,
    /// Same-line trailing comment.
    pub trailing: Option<Comment>,
    /// The call itself.
    pub call: Call,
    /// Optional result binding.
    pub bind: Option<Binding>,
    /// Optional call-site override config line.
    pub config: Option<super::document::ConfigLine>,
}

/// A `spawn name(args…)` statement. A binding parses (so the checker can
/// refuse it with a targeted diagnostic) but is a check error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnStmt {
    /// Span covering the statement line.
    pub span: Span,
    /// Leading trivia before the statement.
    pub lead: Vec<Lead>,
    /// Same-line trailing comment.
    pub trailing: Option<Comment>,
    /// The spawned child call.
    pub call: Call,
    /// A parsed-but-illegal binding, kept for checker diagnostics.
    pub bind: Option<Binding>,
}

/// One stage of a pipe chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipeStage {
    /// A one-argument action or child stage.
    Action {
        /// Source span of the stage name.
        span: Span,
        /// Stage callee name.
        name: String,
    },
    /// A `.field` projection stage.
    Field {
        /// Source span of the accessor.
        span: Span,
        /// Projected field name.
        name: String,
    },
    /// A deterministic combinator stage.
    Combinator(CombinatorCall),
}

/// The fixed combinator vocabulary (deterministic, VM-executed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombinatorKind {
    /// `filter(pred)` — keep matching items.
    Filter,
    /// `map(proj)` — project each item.
    Map,
    /// `sort(key)` — order items by key.
    Sort,
    /// `count` — number of items.
    Count,
    /// `any(pred)` — whether at least one item matches.
    Any,
    /// `all(pred)` — whether every item matches.
    All,
}

/// One combinator invocation in a pipe chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombinatorCall {
    /// Span from the combinator keyword through its argument.
    pub span: Span,
    /// Which combinator.
    pub kind: CombinatorKind,
    /// Optional argument (`.field` accessor or literal).
    pub arg: Option<Expr>,
}

/// How a pipe chain terminates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipeEnd {
    /// `-> name` — bind the piped value.
    Bind(Binding),
    /// `route <target>` — the piped value is the payload.
    Route(RouteTarget),
}

/// A pipe-chain statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipeStmt {
    /// Span covering the statement.
    pub span: Span,
    /// Leading trivia before the statement.
    pub lead: Vec<Lead>,
    /// Same-line trailing comment.
    pub trailing: Option<Comment>,
    /// The value fed into the first stage.
    pub head: Expr,
    /// Chain stages in order (may be empty: `value |> route target`).
    pub stages: Vec<PipeStage>,
    /// Chain terminator.
    pub end: PipeEnd,
}

/// A route destination: a step, a sibling/parent target, or a workflow
/// outcome with an optional constructed payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteTarget {
    /// Span from the target name through the payload, when present.
    pub span: Span,
    /// Target name.
    pub name: String,
    /// Source span of the target name.
    pub name_span: Span,
    /// Optional constructed payload arguments.
    pub payload: Option<Vec<Arg>>,
}

/// A `wait <signal> [timeout D] -> name` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitStmt {
    /// Span covering the statement line.
    pub span: Span,
    /// Leading trivia before the statement.
    pub lead: Vec<Lead>,
    /// Same-line trailing comment.
    pub trailing: Option<Comment>,
    /// Awaited signal name.
    pub signal: String,
    /// Source span of the signal name.
    pub signal_span: Span,
    /// Optional timeout; with it the binding is optional (`T?`).
    pub timeout: Option<DurationLiteral>,
    /// Required payload binding.
    pub bind: Binding,
}

/// A `sleep <duration>` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SleepStmt {
    /// Span covering the statement line.
    pub span: Span,
    /// Leading trivia before the statement.
    pub lead: Vec<Lead>,
    /// Same-line trailing comment.
    pub trailing: Option<Comment>,
    /// Timer duration.
    pub duration: DurationLiteral,
}

/// The header form of a `fork` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkHeader {
    /// `fork item in collection [sequential]` — one branch per item.
    Collection {
        /// Loop variable name.
        var: String,
        /// Source span of the loop variable.
        var_span: Span,
        /// Collection expression.
        collection: Expr,
        /// Whether branches run one at a time in input order.
        sequential: bool,
    },
    /// Bare `fork` — heterogeneous named branches.
    Named,
}

/// The `join` line closing a fork block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinLine {
    /// Span covering the join line.
    pub span: Span,
    /// Leading trivia before the join line.
    pub lead: Vec<Lead>,
    /// Same-line trailing comment.
    pub trailing: Option<Comment>,
    /// Optional joined-results binding (collection form).
    pub bind: Option<Binding>,
}

/// A `fork … join` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkStmt {
    /// Span covering the fork header line.
    pub span: Span,
    /// Leading trivia before the statement.
    pub lead: Vec<Lead>,
    /// Same-line trailing comment on the header.
    pub trailing: Option<Comment>,
    /// Collection or named-branch form.
    pub header: ForkHeader,
    /// Branch statements.
    pub body: Vec<Statement>,
    /// The closing join line.
    pub join: JoinLine,
}

/// The `until` or `max` tail line of a loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopTail {
    /// Span covering the tail line.
    pub span: Span,
    /// Leading trivia before the line.
    pub lead: Vec<Lead>,
    /// Same-line trailing comment.
    pub trailing: Option<Comment>,
    /// The tail expression.
    pub expr: Expr,
}

/// A `loop <name> = <seed> [counting <name>]` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopStmt {
    /// Span covering the loop header line.
    pub span: Span,
    /// Leading trivia before the statement.
    pub lead: Vec<Lead>,
    /// Same-line trailing comment on the header.
    pub trailing: Option<Comment>,
    /// The one value threaded between iterations.
    pub var: String,
    /// Source span of the threaded name.
    pub var_span: Span,
    /// Seed expression for the first iteration.
    pub seed: Expr,
    /// Optional language-owned iteration counter binding.
    pub counter: Option<Binding>,
    /// Body statements, run at least once.
    pub body: Vec<Statement>,
    /// `until` condition, evaluated after each pass (checker requires it).
    pub until: Option<LoopTail>,
    /// `max` ceiling expression (checker requires it; unbounded is illegal).
    pub max: Option<LoopTail>,
}

/// A `route <target>` statement line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteStmt {
    /// Span covering the statement line.
    pub span: Span,
    /// Leading trivia before the statement.
    pub lead: Vec<Lead>,
    /// Same-line trailing comment.
    pub trailing: Option<Comment>,
    /// Where control goes.
    pub target: RouteTarget,
}

/// An `on failure` compensation block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnFailure {
    /// Span covering the `on failure` line.
    pub span: Span,
    /// Leading trivia before the block.
    pub lead: Vec<Lead>,
    /// Same-line trailing comment on the header.
    pub trailing: Option<Comment>,
    /// Compensation statements; must end in a route (checker-enforced).
    pub body: Vec<Statement>,
}

/// Guard of an outcome clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Guard {
    /// `when <expr>` — fires when the expression holds.
    When {
        /// Span from `when` through the expression.
        span: Span,
        /// The guard expression.
        expr: Expr,
    },
    /// `otherwise` — the complement of the preceding arms.
    Otherwise {
        /// Span of the `otherwise` keyword.
        span: Span,
    },
}

/// One `outcome name: <guard>, route <target>` clause of a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutcomeClause {
    /// Span covering the clause (its first line).
    pub span: Span,
    /// Leading trivia before the clause.
    pub lead: Vec<Lead>,
    /// Same-line trailing comment (on the clause's last line).
    pub trailing: Option<Comment>,
    /// Outcome arm name.
    pub name: String,
    /// Source span of the arm name.
    pub name_span: Span,
    /// Evaluation guard.
    pub guard: Guard,
    /// Where control goes when the arm fires.
    pub route: RouteTarget,
}
