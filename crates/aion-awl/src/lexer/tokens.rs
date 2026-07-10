/// A byte and display-position range in the source document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Zero-based byte offset where the span starts.
    pub start: usize,
    /// Zero-based byte offset just after the span ends.
    pub end: usize,
    /// One-based source line where the span starts.
    pub line: usize,
    /// One-based source column where the span starts, counted in characters
    /// (not bytes), so diagnostics stay editor-correct after multibyte
    /// content earlier on the same line.
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

/// AWL rev-2 keyword tokens.
///
/// This is the complete reserved inventory from the AWL-2 spec. Words that
/// were keywords in AWL-0/1 but are gone from rev-2 (`about`, `do`, `as`,
/// `each`, `repeat`, `finish`, `match`, `case`, `parallel`, `race`,
/// `output`, `error`, `up`, `to`, `in`, `order`, `queue`, `fail`) lex as
/// plain identifiers; the parser rejects them with targeted migration
/// diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyword {
    /// `workflow`.
    Workflow,
    /// `input`.
    Input,
    /// `signal`.
    Signal,
    /// `outcome`.
    Outcome,
    /// `type`.
    Type,
    /// `schema`.
    Schema,
    /// `worker`.
    Worker,
    /// `action`.
    Action,
    /// `child`.
    Child,
    /// `step`.
    Step,
    /// `after`.
    After,
    /// `fork`.
    Fork,
    /// `join`.
    Join,
    /// `loop`.
    Loop,
    /// `counting`.
    Counting,
    /// `until`.
    Until,
    /// `max`.
    Max,
    /// `sequential`.
    Sequential,
    /// `spawn`.
    Spawn,
    /// `wait`.
    Wait,
    /// `sleep`.
    Sleep,
    /// `timeout`.
    Timeout,
    /// `retry`.
    Retry,
    /// `every`.
    Every,
    /// `backoff`.
    Backoff,
    /// `node`.
    Node,
    /// `on`.
    On,
    /// `failure`.
    Failure,
    /// `when`.
    When,
    /// `otherwise`.
    Otherwise,
    /// `route`.
    Route,
    /// `success`.
    Success,
    /// `filter`.
    Filter,
    /// `map`.
    Map,
    /// `sort`.
    Sort,
    /// `count`.
    Count,
    /// `is`.
    Is,
    /// `empty`.
    Empty,
    /// `present`.
    Present,
    /// `absent`.
    Absent,
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
}

impl Keyword {
    /// Look up the keyword for a lexed `snake_case` word, if it is reserved.
    #[must_use]
    pub fn from_word(text: &str) -> Option<Self> {
        match text {
            "workflow" => Some(Self::Workflow),
            "input" => Some(Self::Input),
            "signal" => Some(Self::Signal),
            "outcome" => Some(Self::Outcome),
            "type" => Some(Self::Type),
            "schema" => Some(Self::Schema),
            "worker" => Some(Self::Worker),
            "action" => Some(Self::Action),
            "child" => Some(Self::Child),
            "step" => Some(Self::Step),
            "after" => Some(Self::After),
            "fork" => Some(Self::Fork),
            "join" => Some(Self::Join),
            "loop" => Some(Self::Loop),
            "counting" => Some(Self::Counting),
            "until" => Some(Self::Until),
            "max" => Some(Self::Max),
            "sequential" => Some(Self::Sequential),
            "spawn" => Some(Self::Spawn),
            "wait" => Some(Self::Wait),
            "sleep" => Some(Self::Sleep),
            "timeout" => Some(Self::Timeout),
            "retry" => Some(Self::Retry),
            "every" => Some(Self::Every),
            "backoff" => Some(Self::Backoff),
            "node" => Some(Self::Node),
            "on" => Some(Self::On),
            "failure" => Some(Self::Failure),
            "when" => Some(Self::When),
            "otherwise" => Some(Self::Otherwise),
            "route" => Some(Self::Route),
            "success" => Some(Self::Success),
            "filter" => Some(Self::Filter),
            "map" => Some(Self::Map),
            "sort" => Some(Self::Sort),
            "count" => Some(Self::Count),
            "is" => Some(Self::Is),
            "empty" => Some(Self::Empty),
            "present" => Some(Self::Present),
            "absent" => Some(Self::Absent),
            "not" => Some(Self::Not),
            "and" => Some(Self::And),
            "or" => Some(Self::Or),
            "true" => Some(Self::True),
            "false" => Some(Self::False),
            _ => None,
        }
    }
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

/// AWL rev-2 token kinds.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    /// A reserved keyword.
    Keyword(Keyword),
    /// A `snake_case` identifier.
    Identifier(String),
    /// A `TitleCase` type or constructor identifier.
    TypeIdentifier(String),
    /// A `.field` accessor: a dot immediately followed by a `snake_case`
    /// field name (`workspace.branch`, `filter(.blocking)`). The payload is
    /// the field name without the dot.
    FieldAccessor(String),
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
    /// `->`.
    Arrow,
    /// `|>`.
    Pipe,
    /// `|` (enum variant separator).
    Bar,
    /// `?` (postfix type optionality).
    Question,
    /// `=` (loop seed and `type X = …` binder).
    Equal,
    /// `..` (backoff duration range).
    DotDot,
    /// `+` (string concatenation).
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
    /// A `//!` workflow-narration doc line: DATA, not trivia. The payload is
    /// the text after `//!`, verbatim (leading space preserved), so the
    /// printer can round-trip the line byte-for-byte.
    DocHeader(String),
    /// A `///` declaration doc line: DATA, not trivia. The payload is the
    /// text after `///`, verbatim (leading space preserved).
    DocLine(String),
    /// A `//` source comment without the marker or leading single space.
    Comment(String),
    /// The verbatim body of an inline `schema { … }` type door, including the
    /// enclosing braces. The lexer captures the brace-balanced region raw
    /// (string-aware, so braces inside JSON strings do not count) and never
    /// tokenizes it: legal JSON Schema — negative numbers, exponent literals,
    /// `\uXXXX` and `\/` string escapes, any indentation — passes through
    /// byte-for-byte for the parser to validate as JSON and the printer to
    /// re-emit losslessly ("paste an existing JSON Schema verbatim").
    SchemaBody(String),
    /// Increase in two-space indentation level.
    Indent,
    /// Decrease in two-space indentation level.
    Dedent,
}
