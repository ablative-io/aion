//! Shared span mapping for JSON parse failures inside verbatim
//! brace-balanced bodies (the inline `schema { … }` type door and the
//! `json { … }` literal): a `serde_json` line/column becomes a
//! document-correct [`Span`] pointing into the body, plus a short detail
//! naming the offending lexeme when one is under the cursor.

use crate::Span;

/// Map a `serde_json` failure inside `body` (whose braced text occupies
/// `body_span` in the document) to a document-correct span and a detail
/// string. The span points at the offending position INSIDE the body; the
/// detail is `` unexpected `lexeme` `` when an identifier-shaped lexeme sits
/// there, the raw `serde_json` rendering otherwise.
pub(crate) fn json_error_anchor(
    body: &str,
    body_span: Span,
    error: &serde_json::Error,
) -> (Span, String) {
    let (line_in_body, column_in_body) = (error.line().max(1), error.column().max(1));
    let line = body_span.line + line_in_body - 1;
    let line_start: usize = body
        .split_inclusive('\n')
        .take(line_in_body - 1)
        .map(str::len)
        .sum();
    let line_text = body[line_start..].split('\n').next().unwrap_or_default();
    // serde_json's column counts BYTES; clamp to a char boundary for
    // slicing, keep `start`/`end` byte-true, and report the crate's
    // contract-mandated CHARACTER column.
    let mut at = column_in_body.saturating_sub(1).min(line_text.len());
    while at > 0 && !line_text.is_char_boundary(at) {
        at -= 1;
    }
    let lexeme: String = line_text[at..]
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect();
    let start = if line_in_body == 1 {
        body_span.start + at
    } else {
        body_span.start + line_start + at
    };
    let chars_before = line_text[..at].chars().count();
    let column = if line_in_body == 1 {
        body_span.column + chars_before
    } else {
        chars_before + 1
    };
    let span = Span {
        start,
        end: start + lexeme.len().max(1),
        line,
        column,
    };
    let detail = if lexeme.is_empty() {
        error.to_string()
    } else {
        format!("unexpected `{lexeme}`")
    };
    (span, detail)
}
