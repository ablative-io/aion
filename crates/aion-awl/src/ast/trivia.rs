use crate::Span;

/// One `//` source comment, normalized to its text: the marker and one
/// leading space are stripped by the lexer; the printer re-renders the
/// comment as `// {text}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// Source span from the `//` marker through the end of the line.
    pub span: Span,
    /// Comment text without the marker or one leading space.
    pub text: String,
}

/// One `///` doc line attached to a declaration (type, field, action, step)
/// or one `//!` narration line at the top of the document. Doc lines are
/// data, not trivia: they flow into derived JSON Schema `description`s and
/// console narration. The text is verbatim after the marker (leading space
/// preserved) so printing round-trips the line byte-for-byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocLine {
    /// Source span from the marker through the end of the line.
    pub span: Span,
    /// Verbatim text following the `///` or `//!` marker.
    pub text: String,
}

/// One unit of leading trivia before a printable item, in source order.
///
/// Blank lines and own-line comments both belong to the item they precede;
/// runs of consecutive blank lines are canonicalized to a single [`Lead::Blank`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lead {
    /// One or more blank source lines, canonicalized to one.
    Blank,
    /// An own-line `//` comment.
    Comment(Comment),
}
