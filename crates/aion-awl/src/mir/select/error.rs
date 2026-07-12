//! BC-3 selection errors (AWL-BC-IR.md §11). `select` is total for the MIR
//! shapes the covered fixtures produce; anything outside that surface is an
//! honest, span-anchored refusal (`Unsupported`, the D-BC3 stopgap posture),
//! and any encoder/loader/validator rejection surfaces as a hard error — never
//! a silent artifact (§11.5 self-gate).

use beamr::loader::encode::EncodeError;

use crate::mir::Span;

/// A BC-3 selection failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectError {
    /// A MIR op/shell/tail shape BC-3 does not yet lower to instructions. Per
    /// D-BC3 (parity-first) these are the not-yet-reachable rows of §11.4,
    /// documented and span-anchored — never a skip, never a silent wrong emit.
    Unsupported { what: String, span: Span },
    /// An emit-time capability cap was hit (X < 256, arity <= 255 — §11.2).
    OutOfRange { what: String },
    /// A structural invariant of the MIR was violated (a bug upstream, surfaced
    /// rather than swallowed).
    Invariant { what: String },
    /// The beamr encoder rejected the assembled module (§11.5).
    Encode(EncodeError),
    /// The self-gate load (`load_beam_chunks`) rejected the emitted bytes.
    Load(String),
    /// The self-gate `validate_module` rejected the emitted module (one of the
    /// five loader layers — the BC-3 oracle).
    Validate(String),
}

impl SelectError {
    pub(crate) fn unsupported(what: impl Into<String>, span: Span) -> Self {
        Self::Unsupported {
            what: what.into(),
            span,
        }
    }

    pub(crate) fn invariant(what: impl Into<String>) -> Self {
        Self::Invariant { what: what.into() }
    }
}

impl std::fmt::Display for SelectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported { what, span } => write!(
                formatter,
                "BC-3 does not yet emit `{what}` (line {}, col {})",
                span.line, span.column
            ),
            Self::OutOfRange { what } => write!(formatter, "BC-3 emit cap exceeded: {what}"),
            Self::Invariant { what } => write!(formatter, "BC-3 MIR invariant violated: {what}"),
            Self::Encode(error) => write!(formatter, "beamr encode rejected the module: {error}"),
            Self::Load(message) => {
                write!(formatter, "self-gate load rejected the bytes: {message}")
            }
            Self::Validate(message) => {
                write!(
                    formatter,
                    "self-gate validate rejected the module: {message}"
                )
            }
        }
    }
}

impl std::error::Error for SelectError {}

impl From<EncodeError> for SelectError {
    fn from(error: EncodeError) -> Self {
        Self::Encode(error)
    }
}
