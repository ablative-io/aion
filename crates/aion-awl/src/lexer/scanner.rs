use super::{DurationUnit, Keyword, LexError, Span, Token, TokenKind};

/// Lex an AWL rev-2 source document into tokens.
///
/// Blank lines and whole-line `//` trivia comments do not produce `Newline`
/// or indentation changes. Whole-line doc lines (`//!`, `///`) are data: they
/// produce their doc token followed by a `Newline`, but never adjust the
/// indentation stack (attachment by position is the parser's job). Non-blank
/// code lines produce a trailing `Newline` token when the line has a physical
/// line ending.
///
/// # Errors
///
/// Returns the first lexical error, including unterminated strings,
/// unsupported string escapes, tabs in indentation, non-two-space
/// indentation, a dot without a field name, and stray characters.
pub fn lex(source: &str) -> Result<Vec<Token>, LexError> {
    let mut lexer = Lexer::new(source);
    lexer.lex()?;
    Ok(lexer.tokens)
}

/// The classification of a `//`-prefixed line or line tail.
enum CommentKind {
    /// `//!` workflow narration: data.
    DocHeader,
    /// `///` declaration doc (but not `////…`): data.
    DocLine,
    /// Plain `//` trivia.
    Trivia,
}

/// Classify a comment starting at `text` (which begins with `//`) and return
/// the marker length together with its kind.
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
        for raw_line in self.source.split_inclusive('\n') {
            self.lex_line(raw_line)?;
            self.offset += raw_line.len();
            if raw_line.ends_with('\n') {
                self.line += 1;
            }
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

    fn lex_line(&mut self, raw_line: &str) -> Result<(), LexError> {
        let has_newline = raw_line.ends_with('\n');
        let mut line_text = if has_newline {
            &raw_line[..raw_line.len() - 1]
        } else {
            raw_line
        };
        if let Some(stripped) = line_text.strip_suffix('\r') {
            line_text = stripped;
        }

        let content_start = count_leading_spaces(line_text, self.offset, self.line)?;
        let after_indent = &line_text[content_start..];
        if after_indent.trim().is_empty() {
            return Ok(());
        }
        if after_indent.starts_with("//") {
            let is_data = self.push_comment_token(line_text, content_start);
            if is_data && has_newline {
                self.push_line_newline(line_text);
            }
            return Ok(());
        }
        if content_start % 2 != 0 {
            let span = Span::new(self.offset, self.offset + content_start, self.line, 1);
            return Err(LexError::new(span, "indentation must use two-space levels"));
        }

        self.adjust_indent(content_start)?;

        let mut cursor = Cursor::new(line_text, self.offset, self.line, content_start);

        while !cursor.is_at_end() {
            cursor.skip_inline_whitespace();
            if cursor.is_at_end() {
                break;
            }

            if cursor.starts_with("//") {
                self.push_comment_token(line_text, cursor.index);
                break;
            }

            let token = cursor.next_token()?;
            self.tokens.push(token);
        }

        if has_newline {
            self.push_line_newline(line_text);
        }

        Ok(())
    }

    /// Push the token for a comment or doc line starting at `start` within
    /// `line_text`, and report whether it was a data token (doc line).
    fn push_comment_token(&mut self, line_text: &str, start: usize) -> bool {
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
                self.offset + start,
                self.offset + line_text.len(),
                self.line,
                start + 1,
            ),
        ));
        is_data
    }

    fn push_line_newline(&mut self, line_text: &str) {
        self.tokens.push(Token::new(
            TokenKind::Newline,
            Span::new(
                self.offset + line_text.len(),
                self.offset + line_text.len() + 1,
                self.line,
                line_text.len() + 1,
            ),
        ));
    }

    fn adjust_indent(&mut self, spaces: usize) -> Result<(), LexError> {
        let current = *self.indents.last().unwrap_or(&0);
        match spaces.cmp(&current) {
            std::cmp::Ordering::Greater => {
                let mut level = current + 2;
                while level <= spaces {
                    self.indents.push(level);
                    self.tokens.push(Token::new(
                        TokenKind::Indent,
                        Span::new(self.offset, self.offset + spaces, self.line, 1),
                    ));
                    level += 2;
                }
            }
            std::cmp::Ordering::Less => {
                while self.indents.last().copied().unwrap_or(0) > spaces {
                    self.indents.pop();
                    self.tokens.push(Token::new(
                        TokenKind::Dedent,
                        Span::new(self.offset, self.offset, self.line, 1),
                    ));
                }
                if self.indents.last().copied().unwrap_or(0) != spaces {
                    return Err(LexError::new(
                        Span::new(self.offset, self.offset + spaces, self.line, 1),
                        "dedent does not match a previous indentation level",
                    ));
                }
            }
            std::cmp::Ordering::Equal => {}
        }
        Ok(())
    }
}

struct Cursor<'line> {
    line: &'line str,
    base: usize,
    line_no: usize,
    index: usize,
}

impl<'line> Cursor<'line> {
    const fn new(line: &'line str, base: usize, line_no: usize, index: usize) -> Self {
        Self {
            line,
            base,
            line_no,
            index,
        }
    }

    fn is_at_end(&self) -> bool {
        self.index >= self.line.len()
    }

    fn starts_with(&self, needle: &str) -> bool {
        self.line[self.index..].starts_with(needle)
    }

    fn skip_inline_whitespace(&mut self) {
        while matches!(self.current(), Some(' ')) {
            self.advance_char();
        }
    }

    fn current(&self) -> Option<char> {
        self.line[self.index..].chars().next()
    }

    fn peek_after_current(&self) -> Option<char> {
        let mut chars = self.line[self.index..].chars();
        chars.next()?;
        chars.next()
    }

    fn next_token(&mut self) -> Result<Token, LexError> {
        let start = self.index;
        let ch = self.advance_char().ok_or_else(|| {
            LexError::new(self.span(start, start), "unexpected end of source line")
        })?;

        match ch {
            '(' => Ok(self.simple(TokenKind::LeftParen, start)),
            ')' => Ok(self.simple(TokenKind::RightParen, start)),
            '{' => Ok(self.simple(TokenKind::LeftBrace, start)),
            '}' => Ok(self.simple(TokenKind::RightBrace, start)),
            '[' => Ok(self.simple(TokenKind::LeftBracket, start)),
            ']' => Ok(self.simple(TokenKind::RightBracket, start)),
            ':' => Ok(self.simple(TokenKind::Colon, start)),
            ',' => Ok(self.simple(TokenKind::Comma, start)),
            '+' => Ok(self.simple(TokenKind::Plus, start)),
            '?' => Ok(self.simple(TokenKind::Question, start)),
            '.' if self.consume_if('.') => Ok(self.spanned(TokenKind::DotDot, start, self.index)),
            '.' => self.field_accessor(start),
            '-' if self.consume_if('>') => Ok(self.spanned(TokenKind::Arrow, start, self.index)),
            '|' if self.consume_if('>') => Ok(self.spanned(TokenKind::Pipe, start, self.index)),
            '|' => Ok(self.simple(TokenKind::Bar, start)),
            '=' if self.consume_if('=') => {
                Ok(self.spanned(TokenKind::EqualEqual, start, self.index))
            }
            '=' => Ok(self.simple(TokenKind::Equal, start)),
            '!' if self.consume_if('=') => {
                Ok(self.spanned(TokenKind::BangEqual, start, self.index))
            }
            '<' if self.consume_if('=') => {
                Ok(self.spanned(TokenKind::LessEqual, start, self.index))
            }
            '<' => Ok(self.simple(TokenKind::Less, start)),
            '>' if self.consume_if('=') => {
                Ok(self.spanned(TokenKind::GreaterEqual, start, self.index))
            }
            '>' => Ok(self.simple(TokenKind::Greater, start)),
            '"' => self.string(start),
            ch if ch.is_ascii_digit() => self.number(start),
            ch if ch.is_ascii_lowercase() => Ok(self.word(start, false)),
            ch if ch.is_ascii_uppercase() => Ok(self.word(start, true)),
            _ => Err(LexError::new(
                self.span(start, self.index),
                format!("stray character `{ch}`"),
            )),
        }
    }

    fn simple(&self, kind: TokenKind, start: usize) -> Token {
        self.spanned(kind, start, self.index)
    }

    fn spanned(&self, kind: TokenKind, start: usize, end: usize) -> Token {
        Token::new(kind, self.span(start, end))
    }

    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.base + start, self.base + end, self.line_no, start + 1)
    }

    fn advance_char(&mut self) -> Option<char> {
        let ch = self.current()?;
        self.index += ch.len_utf8();
        Some(ch)
    }

    fn consume_if(&mut self, expected: char) -> bool {
        if self.current() == Some(expected) {
            self.advance_char();
            true
        } else {
            false
        }
    }

    /// Lex a `.field` accessor. The dot has already been consumed and sits at
    /// `start`; the field name must follow immediately, `snake_case`.
    fn field_accessor(&mut self, start: usize) -> Result<Token, LexError> {
        if !self.current().is_some_and(|ch| ch.is_ascii_lowercase()) {
            return Err(LexError::new(
                self.span(start, self.index),
                "expected a field name after `.`",
            ));
        }
        while matches!(self.current(), Some(ch) if is_identifier_continue(ch)) {
            self.advance_char();
        }
        let name = &self.line[start + 1..self.index];
        Ok(self.spanned(TokenKind::FieldAccessor(name.to_owned()), start, self.index))
    }

    fn string(&mut self, start: usize) -> Result<Token, LexError> {
        let mut value = String::new();
        while let Some(ch) = self.advance_char() {
            match ch {
                '"' => return Ok(self.spanned(TokenKind::String(value), start, self.index)),
                '\\' => {
                    let escape_start = self.index - 1;
                    let escaped = self.advance_char().ok_or_else(|| {
                        LexError::new(self.span(start, self.index), "unterminated string literal")
                    })?;
                    match escaped {
                        '"' => value.push('"'),
                        '\\' => value.push('\\'),
                        'n' => value.push('\n'),
                        't' => value.push('\t'),
                        _ => {
                            return Err(LexError::new(
                                self.span(escape_start, self.index),
                                format!("unsupported string escape `\\{escaped}`"),
                            ));
                        }
                    }
                }
                _ => value.push(ch),
            }
        }

        Err(LexError::new(
            self.span(start, self.index),
            "unterminated string literal",
        ))
    }

    fn number(&mut self, start: usize) -> Result<Token, LexError> {
        while matches!(self.current(), Some(ch) if ch.is_ascii_digit()) {
            self.advance_char();
        }

        let number_end = self.index;
        if let Some(unit) = self.current().and_then(duration_unit) {
            let after_unit = self.index + 1;
            if !self.line[after_unit..]
                .chars()
                .next()
                .is_some_and(is_identifier_continue)
            {
                self.advance_char();
                let magnitude =
                    parse_u64(&self.line[start..number_end], self.span(start, number_end))?;
                return Ok(self.spanned(
                    TokenKind::Duration { magnitude, unit },
                    start,
                    self.index,
                ));
            }
        }

        if self.current() == Some('.')
            && self
                .peek_after_current()
                .is_some_and(|ch| ch.is_ascii_digit())
        {
            self.advance_char();
            while matches!(self.current(), Some(ch) if ch.is_ascii_digit()) {
                self.advance_char();
            }
            let lexeme = &self.line[start..self.index];
            if lexeme.parse::<f64>().is_err() {
                return Err(LexError::new(
                    self.span(start, self.index),
                    "invalid float literal",
                ));
            }
            return Ok(self.spanned(TokenKind::Float(lexeme.to_owned()), start, self.index));
        }

        let value = parse_u64(&self.line[start..number_end], self.span(start, number_end))?;
        Ok(self.spanned(TokenKind::Integer(value), start, number_end))
    }

    fn word(&mut self, start: usize, type_name: bool) -> Token {
        while matches!(self.current(), Some(ch) if is_identifier_continue(ch)) {
            self.advance_char();
        }
        let text = &self.line[start..self.index];
        if !type_name {
            if let Some(keyword) = Keyword::from_word(text) {
                return self.spanned(TokenKind::Keyword(keyword), start, self.index);
            }
            return self.spanned(TokenKind::Identifier(text.to_owned()), start, self.index);
        }
        self.spanned(
            TokenKind::TypeIdentifier(text.to_owned()),
            start,
            self.index,
        )
    }
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

fn is_identifier_continue(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn normalize_comment_text(text: &str) -> &str {
    text.strip_prefix(' ').unwrap_or(text)
}

fn duration_unit(ch: char) -> Option<DurationUnit> {
    match ch {
        's' => Some(DurationUnit::Seconds),
        'm' => Some(DurationUnit::Minutes),
        'h' => Some(DurationUnit::Hours),
        'd' => Some(DurationUnit::Days),
        _ => None,
    }
}

fn parse_u64(text: &str, span: Span) -> Result<u64, LexError> {
    text.parse::<u64>()
        .map_err(|_| LexError::new(span, "integer literal is too large"))
}
