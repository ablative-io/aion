//! Lexer for the AWL workflow language.
//!
//! The lexer is intentionally hand-written so the parser can rely on exact token
//! spans, indentation tokens, and AWL's `about` prose mode.

#![allow(clippy::module_name_repetitions)]

mod ast;
mod checker;
mod emitter;
mod parser;
mod printer;

pub use ast::{
    AboutDecl, ActionDecl, ActionFieldTag, BinaryOp, BindDecl, CallExpr, CallTarget, Comment,
    Document, DurationLiteral, EachSpec, Expr, FieldDecl, HandlerBlock, HandlerTerminal, IoDecl,
    RecordField, RetrySpec, Spanned, StepDecl, StepFieldTag, StepOp, Trivia, TypeDecl, TypeRef,
    WorkflowDecl,
};
pub use checker::{CheckError, check};
pub use emitter::{EmitError, emit};
pub use parser::{ParseError, parse};
pub use printer::print;

use std::error::Error;
use std::fmt;

/// A byte and display-position range in the source document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Zero-based byte offset where the span starts.
    pub start: usize,
    /// Zero-based byte offset just after the span ends.
    pub end: usize,
    /// One-based source line where the span starts.
    pub line: usize,
    /// One-based source column where the span starts.
    pub column: usize,
}

impl Span {
    const fn new(start: usize, end: usize, line: usize, column: usize) -> Self {
        Self {
            start,
            end,
            line,
            column,
        }
    }
}

/// One lexical token with its source span.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    /// The token's lexical kind and value, when the token carries one.
    pub kind: TokenKind,
    /// The token's source span.
    pub span: Span,
}

impl Token {
    const fn new(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }
}

/// AWL keyword tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyword {
    /// `workflow`.
    Workflow,
    /// `about`.
    About,
    /// `input`.
    Input,
    /// `output`.
    Output,
    /// `error`.
    Error,
    /// `signal`.
    Signal,
    /// `type`.
    Type,
    /// `action`.
    Action,
    /// `step`.
    Step,
    /// `finish`.
    Finish,
    /// `when`.
    When,
    /// `each`.
    Each,
    /// `in`.
    In,
    /// `do`.
    Do,
    /// `child`.
    Child,
    /// `wait`.
    Wait,
    /// `sleep`.
    Sleep,
    /// `repeat`.
    Repeat,
    /// `up`.
    Up,
    /// `to`.
    To,
    /// `until`.
    Until,
    /// `retry`.
    Retry,
    /// `every`.
    Every,
    /// `backoff`.
    Backoff,
    /// `timeout`.
    Timeout,
    /// `on`.
    On,
    /// `failure`.
    Failure,
    /// `as`.
    As,
    /// `queue`.
    Queue,
    /// `node`.
    Node,
    /// `not`.
    Not,
    /// `and`.
    And,
    /// `or`.
    Or,
    /// `true`.
    True,
    /// `false`.
    False,
    /// `fail`.
    Fail,
}

/// A duration unit suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurationUnit {
    /// Seconds, `s`.
    Seconds,
    /// Minutes, `m`.
    Minutes,
    /// Hours, `h`.
    Hours,
    /// Days, `d`.
    Days,
}

/// AWL token kinds.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    /// A reserved keyword.
    Keyword(Keyword),
    /// A `snake_case` identifier.
    Identifier(String),
    /// A `TitleCase` type identifier.
    TypeIdentifier(String),
    /// Prose captured after an `about` keyword through the end of the line.
    Prose(String),
    /// A string literal after escape processing.
    String(String),
    /// An integer literal.
    Integer(u64),
    /// A floating-point literal, holding the exact source lexeme (e.g.
    /// `"1.0"`, `"0.5"`) so printing can round-trip it byte-for-byte instead
    /// of reformatting through an `f64`.
    Float(String),
    /// An integer duration literal with a unit suffix.
    Duration {
        /// The integer value before the unit suffix.
        magnitude: u64,
        /// The parsed duration unit suffix.
        unit: DurationUnit,
    },
    /// `(`.
    LeftParen,
    /// `)`.
    RightParen,
    /// `{`.
    LeftBrace,
    /// `}`.
    RightBrace,
    /// `[`.
    LeftBracket,
    /// `]`.
    RightBracket,
    /// `:`.
    Colon,
    /// `,`.
    Comma,
    /// `.`.
    Dot,
    /// `->`.
    Arrow,
    /// `..`.
    DotDot,
    /// `+`.
    Plus,
    /// `==`.
    EqualEqual,
    /// `!=`.
    BangEqual,
    /// `<`.
    Less,
    /// `<=`.
    LessEqual,
    /// `>`.
    Greater,
    /// `>=`.
    GreaterEqual,
    /// A significant line break after a non-blank source line.
    Newline,
    /// A `//` source comment without the marker or leading single space.
    Comment(String),
    /// Increase in two-space indentation level.
    Indent,
    /// Decrease in two-space indentation level.
    Dedent,
}

/// A lexer diagnostic with a source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    /// The source span for the offending text.
    pub span: Span,
    /// Human-readable diagnostic text.
    pub message: String,
}

impl LexError {
    fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = &self.message;
        let line = self.span.line;
        let column = self.span.column;
        write!(f, "{message} at line {line}, column {column}")
    }
}

impl Error for LexError {}

/// Lex an AWL source document into tokens.
///
/// Blank lines and whole-line comments do not produce `Newline` or indentation
/// changes. Non-blank lines do produce a trailing `Newline` token when the line
/// has a physical line ending.
///
/// # Errors
///
/// Returns the first lexical error, including unterminated strings, unsupported
/// string escapes, tabs in indentation, non-two-space indentation, and stray
/// characters.
#[allow(clippy::too_many_lines)]
pub fn lex(source: &str) -> Result<Vec<Token>, LexError> {
    let mut lexer = Lexer::new(source);
    lexer.lex()?;
    Ok(lexer.tokens)
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

    #[allow(clippy::too_many_lines)]
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
        if after_indent.trim_start().starts_with("//") {
            let comment_start = content_start + after_indent.find("//").unwrap_or(0);
            let comment_text = normalize_comment_text(&line_text[comment_start + 2..]);
            self.tokens.push(Token::new(
                TokenKind::Comment(comment_text.to_owned()),
                Span::new(
                    self.offset + comment_start,
                    self.offset + line_text.len(),
                    self.line,
                    comment_start + 1,
                ),
            ));
            return Ok(());
        }
        if content_start % 2 != 0 {
            let span = Span::new(self.offset, self.offset + content_start, self.line, 1);
            return Err(LexError::new(span, "indentation must use two-space levels"));
        }

        self.adjust_indent(content_start)?;

        let mut cursor = Cursor::new(line_text, self.offset, self.line, content_start);
        let mut just_emitted_about = false;

        while !cursor.is_at_end() {
            cursor.skip_inline_whitespace();
            if cursor.is_at_end() {
                break;
            }

            if just_emitted_about {
                let prose_start = cursor.index;
                let text = line_text[prose_start..].trim_start_matches(' ');
                let trim_delta = line_text[prose_start..].len() - text.len();
                let span_start = self.offset + prose_start + trim_delta;
                let column = prose_start + trim_delta + 1;
                self.tokens.push(Token::new(
                    TokenKind::Prose(text.to_owned()),
                    Span::new(span_start, self.offset + line_text.len(), self.line, column),
                ));
                break;
            }

            if cursor.starts_with("//") {
                let comment_start = cursor.index;
                let comment_text = normalize_comment_text(&line_text[comment_start + 2..]);
                self.tokens.push(Token::new(
                    TokenKind::Comment(comment_text.to_owned()),
                    Span::new(
                        self.offset + comment_start,
                        self.offset + line_text.len(),
                        self.line,
                        comment_start + 1,
                    ),
                ));
                break;
            }

            let token = cursor.next_token()?;
            just_emitted_about = matches!(token.kind, TokenKind::Keyword(Keyword::About));
            self.tokens.push(token);
        }

        if has_newline {
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

        Ok(())
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

    #[allow(clippy::too_many_lines)]
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
            '.' if self.consume_if('.') => Ok(self.spanned(TokenKind::DotDot, start, self.index)),
            '.' => Ok(self.simple(TokenKind::Dot, start)),
            '-' if self.consume_if('>') => Ok(self.spanned(TokenKind::Arrow, start, self.index)),
            '=' if self.consume_if('=') => {
                Ok(self.spanned(TokenKind::EqualEqual, start, self.index))
            }
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
            if let Some(keyword) = keyword(text) {
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

fn keyword(text: &str) -> Option<Keyword> {
    match text {
        "workflow" => Some(Keyword::Workflow),
        "about" => Some(Keyword::About),
        "input" => Some(Keyword::Input),
        "output" => Some(Keyword::Output),
        "error" => Some(Keyword::Error),
        "signal" => Some(Keyword::Signal),
        "type" => Some(Keyword::Type),
        "action" => Some(Keyword::Action),
        "step" => Some(Keyword::Step),
        "finish" => Some(Keyword::Finish),
        "when" => Some(Keyword::When),
        "each" => Some(Keyword::Each),
        "in" => Some(Keyword::In),
        "do" => Some(Keyword::Do),
        "child" => Some(Keyword::Child),
        "wait" => Some(Keyword::Wait),
        "sleep" => Some(Keyword::Sleep),
        "repeat" => Some(Keyword::Repeat),
        "up" => Some(Keyword::Up),
        "to" => Some(Keyword::To),
        "until" => Some(Keyword::Until),
        "retry" => Some(Keyword::Retry),
        "every" => Some(Keyword::Every),
        "backoff" => Some(Keyword::Backoff),
        "timeout" => Some(Keyword::Timeout),
        "on" => Some(Keyword::On),
        "failure" => Some(Keyword::Failure),
        "as" => Some(Keyword::As),
        "queue" => Some(Keyword::Queue),
        "node" => Some(Keyword::Node),
        "not" => Some(Keyword::Not),
        "and" => Some(Keyword::And),
        "or" => Some(Keyword::Or),
        "true" => Some(Keyword::True),
        "false" => Some(Keyword::False),
        "fail" => Some(Keyword::Fail),
        _ => None,
    }
}
