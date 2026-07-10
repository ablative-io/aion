use super::cursor::{Cursor, column_at};
use super::{Keyword, LexError, Span, Token, TokenKind};

/// Lex an AWL rev-2 source document into tokens.
///
/// Blank lines and whole-line `//` trivia comments do not produce `Newline`
/// or indentation changes. Whole-line doc lines (`//!`, `///`) are data: they
/// produce their doc token followed by a `Newline`, but never adjust the
/// indentation stack (attachment by position is the parser's job). Doc-line
/// classification is whole-line only: `///` or `//!` AFTER code on a line is
/// an ordinary trivia comment, never doc data. Non-blank code lines produce a
/// trailing `Newline` token when the line has a physical line ending.
///
/// An inline `schema {` type door switches the lexer into raw capture: the
/// brace-balanced body (braces included) becomes a single
/// [`TokenKind::SchemaBody`] token, verbatim, however many physical lines it
/// spans — no `Newline`, `Indent`, or `Dedent` tokens are produced inside it,
/// and its content is exempt from every AWL lexical rule. One `Newline` is
/// emitted for the closing-brace line.
///
/// # Errors
///
/// Returns the first lexical error, including unterminated strings and inline
/// schema bodies, unsupported string escapes, tabs in indentation,
/// non-two-space indentation, indentation jumping more than one level, a dot
/// without a field name, and stray characters.
pub fn lex(source: &str) -> Result<Vec<Token>, LexError> {
    let mut lexer = Lexer::new(source);
    lexer.lex()?;
    Ok(lexer.tokens)
}

/// The classification of a whole-line `//`-prefixed comment.
enum CommentKind {
    /// `//!` workflow narration: data.
    DocHeader,
    /// `///` declaration doc (but not `////…`): data.
    DocLine,
    /// Plain `//` trivia.
    Trivia,
}

/// Classify a whole-line comment starting at `text` (which begins with `//`)
/// and return the marker length together with its kind. Only the whole-line
/// path may use this: the spec defines `//!`/`///` doc lines as whole LINES,
/// so a marker trailing code never classifies as data.
fn classify_comment(text: &str) -> (usize, CommentKind) {
    if text.starts_with("//!") {
        return (3, CommentKind::DocHeader);
    }
    if text.starts_with("///") && !text.starts_with("////") {
        return (3, CommentKind::DocLine);
    }
    (2, CommentKind::Trivia)
}

struct Lexer<'src> {
    source: &'src str,
    tokens: Vec<Token>,
    indents: Vec<usize>,
    /// Byte offset of the start of the line currently being lexed.
    offset: usize,
    line: usize,
}

impl<'src> Lexer<'src> {
    fn new(source: &'src str) -> Self {
        Self {
            source,
            tokens: Vec::new(),
            indents: vec![0],
            offset: 0,
            line: 1,
        }
    }

    fn lex(&mut self) -> Result<(), LexError> {
        while self.offset < self.source.len() {
            self.lex_line()?;
        }

        while self.indents.len() > 1 {
            self.indents.pop();
            self.tokens.push(Token::new(
                TokenKind::Dedent,
                Span::new(self.offset, self.offset, self.line, 1),
            ));
        }

        Ok(())
    }

    fn lex_line(&mut self) -> Result<(), LexError> {
        let source = self.source;
        let mut line_start = self.offset;
        let (mut line_text, mut terminator_len, mut has_line_feed) = split_line(source, line_start);

        let content_start = count_leading_spaces(line_text, line_start, self.line)?;
        let after_indent = &line_text[content_start..];
        if after_indent.trim().is_empty() {
            self.finish_line(line_start, line_text, terminator_len, has_line_feed, false);
            return Ok(());
        }
        if after_indent.starts_with("//") {
            let is_data = self.push_line_comment(line_start, line_text, content_start);
            self.finish_line(
                line_start,
                line_text,
                terminator_len,
                has_line_feed,
                is_data,
            );
            return Ok(());
        }
        if content_start % 2 != 0 {
            let span = Span::new(line_start, line_start + content_start, self.line, 1);
            return Err(LexError::new(span, "indentation must use two-space levels"));
        }

        self.adjust_indent(line_start, content_start)?;

        let mut cursor = Cursor::new(line_text, line_start, self.line, content_start);

        loop {
            cursor.skip_inline_whitespace();
            if cursor.is_at_end() {
                break;
            }

            if cursor.starts_with("//") {
                self.push_trailing_comment(&cursor);
                break;
            }

            if cursor.starts_with("{") && self.last_token_is_schema_keyword() {
                let open = cursor.base + cursor.index;
                let open_span = cursor.span(cursor.index, cursor.index + 1);
                let (end, newlines) = scan_schema_body(source, open, open_span)?;
                self.tokens.push(Token::new(
                    TokenKind::SchemaBody(source[open..end].to_owned()),
                    Span::new(open, end, open_span.line, open_span.column),
                ));
                if newlines == 0 {
                    cursor.index = end - cursor.base;
                } else {
                    self.line += newlines;
                    let close_start = source[..end].rfind('\n').map_or(0, |at| at + 1);
                    (line_text, terminator_len, has_line_feed) = split_line(source, close_start);
                    line_start = close_start;
                    cursor = Cursor::new(line_text, close_start, self.line, end - close_start);
                }
                continue;
            }

            let token = cursor.next_token()?;
            self.tokens.push(token);
        }

        self.finish_line(line_start, line_text, terminator_len, has_line_feed, true);
        Ok(())
    }

    /// Close out the physical line starting at `line_start`: emit its
    /// `Newline` token (when asked and the line has a line feed), advance the
    /// line counter, and move `offset` past the terminator.
    fn finish_line(
        &mut self,
        line_start: usize,
        line_text: &str,
        terminator_len: usize,
        has_line_feed: bool,
        emit_newline: bool,
    ) {
        if emit_newline && has_line_feed {
            let newline_start = line_start + line_text.len();
            self.tokens.push(Token::new(
                TokenKind::Newline,
                Span::new(
                    newline_start,
                    newline_start + terminator_len,
                    self.line,
                    column_at(line_text, line_text.len()),
                ),
            ));
        }
        if has_line_feed {
            self.line += 1;
        }
        self.offset = line_start + line_text.len() + terminator_len;
    }

    fn last_token_is_schema_keyword(&self) -> bool {
        matches!(
            self.tokens.last(),
            Some(token) if token.kind == TokenKind::Keyword(Keyword::Schema)
        )
    }

    /// Push the token for a whole-line comment or doc line starting at
    /// `start` within `line_text`, and report whether it was a data token
    /// (doc line).
    fn push_line_comment(&mut self, line_start: usize, line_text: &str, start: usize) -> bool {
        let (marker_len, kind) = classify_comment(&line_text[start..]);
        let text = &line_text[start + marker_len..];
        let (kind, is_data) = match kind {
            CommentKind::DocHeader => (TokenKind::DocHeader(text.to_owned()), true),
            CommentKind::DocLine => (TokenKind::DocLine(text.to_owned()), true),
            CommentKind::Trivia => (
                TokenKind::Comment(normalize_comment_text(text).to_owned()),
                false,
            ),
        };
        self.tokens.push(Token::new(
            kind,
            Span::new(
                line_start + start,
                line_start + line_text.len(),
                self.line,
                column_at(line_text, start),
            ),
        ));
        is_data
    }

    /// Push a `//…` tail after code on a line as a trivia comment. Doc
    /// markers (`///`, `//!`) trailing code are NOT doc data — the spec
    /// defines doc lines as whole lines, so classification is positional: a
    /// trailing marker is an ordinary comment whose text keeps the extra
    /// marker characters.
    fn push_trailing_comment(&mut self, cursor: &Cursor<'_>) {
        let text = &cursor.line[cursor.index + 2..];
        self.tokens.push(Token::new(
            TokenKind::Comment(normalize_comment_text(text).to_owned()),
            Span::new(
                cursor.base + cursor.index,
                cursor.base + cursor.line.len(),
                cursor.line_no,
                column_at(cursor.line, cursor.index),
            ),
        ));
    }

    fn adjust_indent(&mut self, line_start: usize, spaces: usize) -> Result<(), LexError> {
        let current = self.indents.last().copied().unwrap_or(0);
        match spaces.cmp(&current) {
            std::cmp::Ordering::Greater => {
                if spaces > current + 2 {
                    return Err(LexError::new(
                        Span::new(line_start, line_start + spaces, self.line, 1),
                        "indentation increases by more than one two-space level",
                    ));
                }
                self.indents.push(spaces);
                self.tokens.push(Token::new(
                    TokenKind::Indent,
                    Span::new(line_start, line_start + spaces, self.line, 1),
                ));
            }
            std::cmp::Ordering::Less => {
                while self.indents.last().copied().unwrap_or(0) > spaces {
                    self.indents.pop();
                    self.tokens.push(Token::new(
                        TokenKind::Dedent,
                        Span::new(line_start, line_start, self.line, 1),
                    ));
                }
                if self.indents.last().copied().unwrap_or(0) != spaces {
                    return Err(LexError::new(
                        Span::new(line_start, line_start + spaces, self.line, 1),
                        "dedent does not match a previous indentation level",
                    ));
                }
            }
            std::cmp::Ordering::Equal => {}
        }
        Ok(())
    }
}

/// Split the physical line starting at `start`: the content (without its
/// terminator), the terminator's byte length (`\n` → 1, `\r\n` → 2, a bare
/// trailing `\r` at end of input → 1, end of input → 0), and whether the
/// terminator includes a line feed.
fn split_line(source: &str, start: usize) -> (&str, usize, bool) {
    let rest = &source[start..];
    match rest.find('\n') {
        Some(at) => {
            let raw = &rest[..at];
            match raw.strip_suffix('\r') {
                Some(content) => (content, 2, true),
                None => (raw, 1, true),
            }
        }
        None => match rest.strip_suffix('\r') {
            Some(content) => (content, 1, false),
            None => (rest, 0, false),
        },
    }
}

/// Scan the brace-balanced inline `schema { … }` body whose `{` sits at byte
/// `open` in `source`. Braces inside JSON strings do not count; string
/// escapes are skipped blind (which covers `\"`, `\\`, `\uXXXX`, `\/`, and
/// every other JSON escape for balancing purposes); an unclosed quote ends at
/// its line so a typo cannot swallow the rest of the document. Returns the
/// byte offset just past the matching `}` and the number of line feeds
/// crossed.
///
/// # Errors
///
/// Returns an error spanning the opening `{` when the document ends before
/// the matching `}`.
fn scan_schema_body(
    source: &str,
    open: usize,
    open_span: Span,
) -> Result<(usize, usize), LexError> {
    let mut depth = 0usize;
    let mut newlines = 0usize;
    let mut in_string = false;
    let mut chars = source[open..].char_indices();
    while let Some((at, ch)) = chars.next() {
        match ch {
            '\n' => {
                newlines += 1;
                in_string = false;
            }
            '\\' if in_string => {
                if let Some((_, escaped)) = chars.next() {
                    if escaped == '\n' {
                        newlines += 1;
                        in_string = false;
                    }
                }
            }
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Ok((open + at + 1, newlines));
                }
            }
            _ => {}
        }
    }
    Err(LexError::new(
        open_span,
        "unterminated inline `schema { … }` body",
    ))
}

fn count_leading_spaces(line: &str, base: usize, line_no: usize) -> Result<usize, LexError> {
    let mut count = 0;
    for ch in line.chars() {
        match ch {
            ' ' => count += 1,
            '\t' => {
                return Err(LexError::new(
                    Span::new(base + count, base + count + 1, line_no, count + 1),
                    "tabs are not allowed in indentation",
                ));
            }
            _ => break,
        }
    }
    Ok(count)
}

fn normalize_comment_text(text: &str) -> &str {
    text.strip_prefix(' ').unwrap_or(text)
}
