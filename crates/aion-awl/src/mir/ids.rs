//! MIR identity newtypes (AWL-BC-IR.md Appendix A).
//!
//! Single-def `Var`s are per-function; `AtomRef`/`LitRef`/`FnRef` index the
//! module-level atom table, literal pool, and function list. `Span` carries
//! the source line/column that feeds the Line chunk (R4).

/// A single-def variable, unique within one function body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Var(pub u32);

/// Index into [`crate::mir::MirModule::atoms`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AtomRef(pub u32);

/// Index into [`crate::mir::MirModule::literals`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LitRef(pub u32);

/// Index into [`crate::mir::MirModule::functions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FnRef(pub u32);

/// Source position (feeds the Line chunk, R4). Absence is loader-legal, but
/// every op and AST node carries one, so spans are always present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Span {
    pub line: u32,
    pub column: u32,
}

impl Span {
    /// A zero span, for synthesized ops with no direct source position.
    pub(crate) fn zero() -> Self {
        Self { line: 0, column: 0 }
    }

    /// Project a lexer span onto the line/column the Line chunk needs.
    pub(crate) fn from_source(span: crate::Span) -> Self {
        Self {
            line: u32::try_from(span.line).unwrap_or(u32::MAX),
            column: u32::try_from(span.column).unwrap_or(u32::MAX),
        }
    }
}
