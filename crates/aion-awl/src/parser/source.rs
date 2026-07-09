use crate::ast::Comment;
use crate::{LexError, Span, Token, lex};

use super::ParseError;

#[derive(Debug, Clone)]
pub(super) struct SourceLine {
    pub(super) indent: usize,
    pub(super) code: String,
    pub(super) trailing: Option<Comment>,
    pub(super) span: Span,
}

impl SourceLine {
    /// Compute the true document span of `fragment`, which must be a
    /// byte-level subslice of `self.code` (produced via `strip_prefix`,
    /// `trim`, `split_once`, indexing, and similar zero-copy operations).
    ///
    /// Uses pointer arithmetic against `self.code`'s backing buffer, so it
    /// stays correct no matter how many slicing operations were chained to
    /// derive `fragment` — the result is exact even across multi-byte UTF-8
    /// content, since offsets are computed in bytes.
    pub(super) fn fragment_span(&self, fragment: &str) -> Span {
        let code_start = self.code.as_ptr() as usize;
        let fragment_start = fragment.as_ptr() as usize;
        let offset = fragment_start.saturating_sub(code_start);
        span(
            self.span.start + offset,
            self.span.start + offset + fragment.len(),
            self.span.line,
            self.span.column + offset,
        )
    }
}

pub(super) struct SourceLines {
    pub(super) lines: Vec<SourceLine>,
    pub(super) comments: Vec<Comment>,
}

impl SourceLines {
    pub(super) fn new(source: &str) -> Result<Self, ParseError> {
        let mut offset = 0;
        let mut line_no = 1;
        let mut lines = Vec::new();
        let mut comments = Vec::new();
        for raw in source.split_inclusive('\n') {
            let had_newline = raw.ends_with('\n');
            let mut text = if had_newline {
                &raw[..raw.len() - 1]
            } else {
                raw
            };
            if let Some(stripped) = text.strip_suffix('\r') {
                text = stripped;
            }
            let indent = count_indent(text, offset, line_no)?;
            let rest = &text[indent..];
            if rest.trim().is_empty() {
                offset += raw.len();
                if had_newline {
                    line_no += 1;
                }
                continue;
            }
            if let Some(comment_at) = rest.trim_start().strip_prefix("//") {
                let skipped = rest.len() - rest.trim_start().len();
                let start_col = indent + skipped + 1;
                let start = offset + indent + skipped;
                comments.push(Comment {
                    span: span(start, offset + text.len(), line_no, start_col),
                    text: trim_comment(comment_at).to_owned(),
                });
                offset += raw.len();
                if had_newline {
                    line_no += 1;
                }
                continue;
            }
            if indent % 2 != 0 {
                return Err(ParseError::new(
                    span(offset, offset + indent, line_no, 1),
                    "indentation must use two-space levels",
                ));
            }
            let (code, trailing) = split_code_comment(rest, indent, offset, line_no, text.len());
            let code_start = offset + indent;
            let code_end = code_start + code.len();
            lines.push(SourceLine {
                indent,
                code: code.trim_end().to_owned(),
                trailing,
                span: span(code_start, code_end, line_no, indent + 1),
            });
            offset += raw.len();
            if had_newline {
                line_no += 1;
            }
        }
        Ok(Self { lines, comments })
    }
}

fn count_indent(text: &str, offset: usize, line_no: usize) -> Result<usize, ParseError> {
    let mut count = 0;
    for ch in text.chars() {
        match ch {
            ' ' => count += 1,
            '\t' => {
                return Err(ParseError::new(
                    span(offset + count, offset + count + 1, line_no, count + 1),
                    "tabs are not allowed in indentation",
                ));
            }
            _ => break,
        }
    }
    Ok(count)
}

fn split_code_comment(
    rest: &str,
    indent: usize,
    offset: usize,
    line_no: usize,
    line_len: usize,
) -> (String, Option<Comment>) {
    if rest.starts_with("about ") || rest == "about" {
        return (rest.to_owned(), None);
    }
    let mut in_string = false;
    let mut escaped = false;
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        let ch = bytes[i] as char;
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else if ch == '"' {
            in_string = true;
        } else if ch == '/' && bytes[i + 1] as char == '/' {
            let code = rest[..i].trim_end().to_owned();
            let start = offset + indent + i;
            let text = trim_comment(&rest[i + 2..]).to_owned();
            return (
                code,
                Some(Comment {
                    span: span(start, offset + line_len, line_no, indent + i + 1),
                    text,
                }),
            );
        }
        i += 1;
    }
    (rest.to_owned(), None)
}

fn trim_comment(text: &str) -> &str {
    text.strip_prefix(' ').unwrap_or(text)
}
pub(super) fn span(start: usize, end: usize, line: usize, column: usize) -> Span {
    Span {
        start,
        end,
        line,
        column,
    }
}

/// Lex `text`, a fragment of a source line, and rebase every resulting token
/// (and any lexical error) onto its true position in the original document.
///
/// `base` must be `text`'s real document span, as produced by
/// [`SourceLine::fragment_span`]. Fragments never span more than one physical
/// source line, so the rebased line number is always `base.line`.
pub(super) fn lex_at(text: &str, base: Span) -> Result<Vec<Token>, LexError> {
    lex(text)
        .map(|tokens| {
            tokens
                .into_iter()
                .map(|token| Token {
                    span: rebase_span(token.span, base),
                    ..token
                })
                .collect()
        })
        .map_err(|err| LexError {
            span: rebase_span(err.span, base),
            message: err.message,
        })
}

/// Shift a span produced by lexing a fragment in isolation (line 1, column 1
/// at byte 0) onto its true document position, given the fragment's real
/// `base` span.
pub(super) fn rebase_span(fragment_relative: Span, base: Span) -> Span {
    span(
        base.start + fragment_relative.start,
        base.start + fragment_relative.end,
        base.line,
        base.column + fragment_relative.column - 1,
    )
}

pub(super) fn keyword_rest<'a>(
    line: &'a SourceLine,
    keyword: &str,
    message: &str,
) -> Result<&'a str, ParseError> {
    line.code
        .strip_prefix(keyword)
        .map(str::trim_start)
        .ok_or_else(|| ParseError::new(line.span, message))
}
