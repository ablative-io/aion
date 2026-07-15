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
            | Self::RawString { span, .. }
            | Self::Json { span, .. }
            | Self::SchemaOf { span, .. }
            | Self::Int { span, .. }
            | Self::Float { span, .. }
            | Self::Bool { span, .. }
            | Self::List { span, .. }
            | Self::Ref { span, .. }
            | Self::Workflow { span }
            | Self::Variant { span, .. }
            | Self::Record { span, .. }
            | Self::Field { span, .. }
            | Self::Index { span, .. }
            | Self::Accessor { span, .. }
            | Self::Not { span, .. }
            | Self::Binary { span, .. }
            | Self::Predicate { span, .. }
            | Self::CollectionPredicate { span, .. } => *span,
            Self::Duration(duration) => duration.span,
        }
    }
}
