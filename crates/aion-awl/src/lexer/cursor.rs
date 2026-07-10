//! The within-line token scanner for the AWL rev-2 lexer.
//!
//! [`Cursor`] walks one physical source line and produces the ordinary token
//! inventory (keywords, identifiers, operators, literals, `.field`
//! accessors). Line structure — indentation, comments, doc lines, newlines,
//! and inline `schema { … }` raw capture — is the scanner's job.

use super::{DurationUnit, Keyword, LexError, Span, Token, TokenKind};

/// One-based character column of byte index `at` within `line`.
///
/// Columns are counted in characters, not bytes, so diagnostics stay
/// editor-correct after multibyte content earlier on the same line (doc
/// prose routinely carries em dashes and accented text).
pub(super) fn column_at(line: &str, at: usize) -> usize {
    line[..at].chars().count() + 1
}

pub(super) struct Cursor<'line> {
    pub(super) line: &'line str,
    pub(super) base: usize,
    pub(super) line_no: usize,
    pub(super) index: usize,
}

impl<'line> Cursor<'line> {
    pub(super) const fn new(line: &'line str, base: usize, line_no: usize, index: usize) -> Self {
        Self {
            line,
            base,
            line_no,
            index,
        }
    }

    pub(super) fn is_at_end(&self) -> bool {
        self.index >= self.line.len()
    }

    pub(super) fn starts_with(&self, needle: &str) -> bool {
        self.line[self.index..].starts_with(needle)
    }

    pub(super) fn skip_inline_whitespace(&mut self) {
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

    pub(super) fn next_token(&mut self) -> Result<Token, LexError> {
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

    pub(super) fn span(&self, start: usize, end: usize) -> Span {
        Span::new(
            self.base + start,
            self.base + end,
            self.line_no,
            column_at(self.line, start),
        )
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

fn is_identifier_continue(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
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
