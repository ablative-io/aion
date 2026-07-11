use std::path::Path;

use aion_awl::Span;
use lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};

const fn utf16_width(ch: char) -> u32 {
    if ch.len_utf16() == 2 { 2 } else { 1 }
}

/// Converts a byte offset in `source` to a zero-based UTF-16 LSP position.
///
/// Offsets beyond the document are clamped to its end. AWL spans are produced
/// on UTF-8 boundaries; if a caller supplies an interior byte, the position is
/// conservatively placed before that scalar value.
#[must_use]
pub fn position_at(source: &str, byte_offset: usize) -> Position {
    let target = byte_offset.min(source.len());
    let mut line = 0_u32;
    let mut character = 0_u32;
    for (offset, ch) in source.char_indices() {
        if offset >= target || offset.saturating_add(ch.len_utf8()) > target {
            break;
        }
        if ch == '\n' {
            line = line.saturating_add(1);
            character = 0;
        } else {
            character = character.saturating_add(utf16_width(ch));
        }
    }
    Position::new(line, character)
}

/// Converts a zero-based UTF-16 LSP position to a byte offset in `source`.
///
/// Positions past a line or the document are clamped to the nearest end.
#[must_use]
pub fn byte_offset_at(source: &str, position: Position) -> usize {
    let mut line = 0_u32;
    let mut line_start = 0_usize;
    for (offset, ch) in source.char_indices() {
        if line == position.line {
            line_start = offset;
            break;
        }
        if ch == '\n' {
            line = line.saturating_add(1);
            line_start = offset + ch.len_utf8();
        }
    }
    if line < position.line {
        return source.len();
    }

    let mut utf16 = 0_u32;
    for (relative, ch) in source[line_start..].char_indices() {
        if ch == '\n' || utf16 >= position.character {
            return line_start + relative;
        }
        let next = utf16.saturating_add(utf16_width(ch));
        if next > position.character {
            return line_start + relative;
        }
        utf16 = next;
    }
    source.len()
}

/// Converts an AWL byte span to an LSP range against the live document text.
#[must_use]
pub fn range_for_span(source: &str, span: Span) -> Range {
    let start = position_at(source, span.start);
    let end = if span.end <= span.start {
        start
    } else {
        position_at(source, span.end)
    };
    Range::new(start, end)
}

fn diagnostic(source: &str, span: Span, message: String) -> Diagnostic {
    let mut diagnostic = Diagnostic::new_simple(range_for_span(source, span), message);
    diagnostic.severity = Some(DiagnosticSeverity::ERROR);
    diagnostic.source = Some("awl".to_owned());
    diagnostic
}

/// Runs the one real AWL parser and checker and maps their errors to LSP.
///
/// When `root` is present, schema imports resolve relative to the document's
/// directory through `aion_awl::check_in`; otherwise the same checker runs via
/// `aion_awl::check` and reports unresolved imports normally.
#[must_use]
pub fn diagnostics(source: &str, root: Option<&Path>) -> Vec<Diagnostic> {
    let document = match aion_awl::parse(source) {
        Ok(document) => document,
        Err(error) => return vec![diagnostic(source, error.span, error.message)],
    };
    let errors = root.map_or_else(
        || aion_awl::check(&document),
        |directory| aion_awl::check_in(&document, directory),
    );
    errors
        .into_iter()
        .map(|error| diagnostic(source, error.span, error.message))
        .collect()
}

/// Returns the canonical AWL printer output, or `None` when parsing fails.
#[must_use]
pub fn format_document(source: &str) -> Option<String> {
    aion_awl::parse(source)
        .ok()
        .map(|document| aion_awl::print(&document))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_are_utf16_not_bytes_or_scalar_columns() {
        let source = "//! 🧭 café\nworkflow probe\n";
        let workflow = source.find("workflow").unwrap_or_default();
        assert_eq!(position_at(source, workflow), Position::new(1, 0));
        let after_emoji = source.find(" café").unwrap_or_default();
        assert_eq!(position_at(source, after_emoji), Position::new(0, 6));
        assert_eq!(byte_offset_at(source, Position::new(0, 6)), after_emoji);
    }

    #[test]
    fn point_spans_stay_points() {
        let span = Span {
            start: 4,
            end: 4,
            line: 1,
            column: 5,
        };
        let range = range_for_span("café", span);
        assert_eq!(range.start, range.end);
        assert_eq!(range.start.character, 3);
    }
}
