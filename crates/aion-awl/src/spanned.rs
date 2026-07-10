use crate::{Expr, Span};

/// Provides the source span covered by a parsed syntax node.
pub trait Spanned {
    /// Return the source span occupied by this node.
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
