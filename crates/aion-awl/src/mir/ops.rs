//! The closed statement-op set, values, tests, and tails (AWL-BC-IR.md §2.5,
//! §2.7 / Appendix A). Each `Stmt` is one instruction burst with no interior
//! register-pressure decisions; control flow is a tree per function (X2), so
//! nested blocks each end in exactly one `Tail`. Control constructs (`If`,
//! `SelectEnum`) are tails, not statements (S17).

use super::ids::{AtomRef, FnRef, LitRef, Span, Var};
use super::runtime::RuntimeFn;
use super::tydesc::Leaf;

/// A function body: a straight-line statement burst ending in one tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Tail,
}

/// A value operand (no registers — BC-3 owns x/y assignment).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Var(Var),
    Lit(LitRef),
    Atom(AtomRef),
    Int(i64),
    Nil,
}

/// The set of vars live across a call/bind op — the y-spill contract handed
/// to BC-3 as data (S14). Printed in goldens.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LiveAfter(pub Vec<Var>);

/// A comparison operator; the Int/Float split is preserved (`exprs.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    FLt,
    FLe,
    FGt,
    FGe,
}

/// A value-position boolean binary operator (S4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoolBin {
    And,
    Or,
}

/// A `json.object` pair value, encoded via a leaf or module-local `to_json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonVal {
    Encoded { value: Value, via: ToJsonRef },
}

/// The `to_json` function a [`JsonVal`] flows through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToJsonRef {
    SdkLeaf(Leaf),
    Local(FnRef),
}

/// A test-position predicate; short-circuiting is nested `If` tails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Test {
    IsTrue(Value),
    Cmp {
        op: CmpOp,
        lhs: Value,
        rhs: Value,
    },
    IsTagged {
        value: Value,
        tag: AtomRef,
        arity: u16,
    },
    Not(Box<Test>),
}

/// One instruction burst. Each op defines at most one fresh `Var`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    Bind {
        dst: Var,
        value: Value,
        span: Span,
    },
    FieldGet {
        dst: Var,
        base: Value,
        index: u16,
        span: Span,
    },
    RecordNew {
        dst: Var,
        tag: AtomRef,
        args: Vec<Value>,
        span: Span,
    },
    /// An untagged tuple (`#(a, b)` — the counted-loop `Ok(#(value, count))`
    /// result). Distinct from `RecordNew`: no tag atom occupies element 0, so
    /// the Gleam tuple ABI (`TyDesc::Tuple`) is honored exactly.
    TupleNew {
        dst: Var,
        items: Vec<Value>,
        span: Span,
    },
    ListNew {
        dst: Var,
        items: Vec<Value>,
        span: Span,
    },
    CallRt {
        dst: Option<Var>,
        callee: RuntimeFn,
        args: Vec<Value>,
        live_after: LiveAfter,
        span: Span,
    },
    CallLocal {
        dst: Option<Var>,
        callee: FnRef,
        args: Vec<Value>,
        live_after: LiveAfter,
        span: Span,
    },
    CallClosure {
        dst: Option<Var>,
        fun: Value,
        args: Vec<Value>,
        live_after: LiveAfter,
        span: Span,
    },
    MakeClosure {
        dst: Var,
        lifted: FnRef,
        captures: Vec<Value>,
        span: Span,
    },
    TryBind {
        dst: Var,
        result: Var,
        live_after: LiveAfter,
        span: Span,
    },
    WaitTimeoutCase {
        dst: Var,
        receive: FnRef,
        captures: Vec<Value>,
        deadline_ms: u64,
        span: Span,
    },
    Cmp {
        dst: Var,
        op: CmpOp,
        lhs: Value,
        rhs: Value,
        span: Span,
    },
    BoolOp {
        dst: Var,
        op: BoolBin,
        lhs: Value,
        rhs: Value,
        span: Span,
    },
    Not {
        dst: Var,
        src: Value,
        span: Span,
    },
    Concat {
        dst: Var,
        lhs: Value,
        rhs: Value,
        span: Span,
    },
    Increment {
        dst: Var,
        src: Var,
        span: Span,
    },
    AssertList {
        binds: Vec<Option<Var>>,
        list: Var,
        span: Span,
    },
    AssertSome {
        dst: Var,
        option: Var,
        span: Span,
    },
    JsonObj {
        dst: Var,
        pairs: Vec<(String, JsonVal)>,
        span: Span,
    },
    IndexGuard {
        dst: Var,
        base: Var,
        index: u64,
        message: String,
        span: Span,
    },
    Attempt {
        lifted: FnRef,
        captures: Vec<Value>,
        defs: Vec<Var>,
        on_ok: Block,
        on_err: Block,
        span: Span,
    },
}

/// The tail of a block: a return, a tail call, or a control construct whose
/// arms each end in their own tail (S17).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tail {
    Return(Value),
    TailLocal {
        callee: FnRef,
        args: Vec<Value>,
    },
    TailRt {
        callee: RuntimeFn,
        args: Vec<Value>,
    },
    If {
        test: Test,
        then_block: Box<Block>,
        else_block: Box<Block>,
        span: Span,
    },
    SelectEnum {
        subject: Value,
        arms: Vec<(AtomRef, Block)>,
        span: Span,
    },
}

impl Stmt {
    /// The single var this op defines, when it defines one (for single-def and
    /// liveness checks in `verify`).
    pub(crate) fn defined(&self) -> Option<Var> {
        match self {
            Self::Bind { dst, .. }
            | Self::FieldGet { dst, .. }
            | Self::RecordNew { dst, .. }
            | Self::TupleNew { dst, .. }
            | Self::ListNew { dst, .. }
            | Self::MakeClosure { dst, .. }
            | Self::TryBind { dst, .. }
            | Self::WaitTimeoutCase { dst, .. }
            | Self::Cmp { dst, .. }
            | Self::BoolOp { dst, .. }
            | Self::Not { dst, .. }
            | Self::Concat { dst, .. }
            | Self::Increment { dst, .. }
            | Self::AssertSome { dst, .. }
            | Self::JsonObj { dst, .. }
            | Self::IndexGuard { dst, .. } => Some(*dst),
            Self::CallRt { dst, .. }
            | Self::CallLocal { dst, .. }
            | Self::CallClosure { dst, .. } => *dst,
            Self::AssertList { .. } | Self::Attempt { .. } => None,
        }
    }

    /// The runtime callee this op invokes, when it is a durable call (for the
    /// S11 effect schedule and S16 capability summary).
    pub(crate) fn runtime_callee(&self) -> Option<RuntimeFn> {
        match self {
            Self::CallRt { callee, .. } => Some(*callee),
            _ => None,
        }
    }
}
