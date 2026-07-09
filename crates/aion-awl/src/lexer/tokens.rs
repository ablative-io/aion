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
    pub(super) const fn new(start: usize, end: usize, line: usize, column: usize) -> Self {
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
    pub(super) const fn new(kind: TokenKind, span: Span) -> Self {
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
