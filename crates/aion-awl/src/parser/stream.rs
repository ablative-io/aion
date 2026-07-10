//! Token-stream machinery for the rev-2 parser: lookahead, line/block
//! structure, and lossless trivia collection (blank lines, comments, docs).

use crate::ast::{Comment, DocLine, Lead};
use crate::{Span, Token, TokenKind};

use super::ParseError;

pub(super) struct Stream {
    tokens: Vec<Token>,
    pos: usize,
    /// Line of the last consumed non-structural token (`Indent`/`Dedent` are
    /// synthetic and do not advance this), used to detect blank lines: the
    /// lexer emits nothing for a blank line, so a gap in line numbers between
    /// consecutive real tokens is the only witness.
    last_line: usize,
    /// Leads collected inside a block that turned out to belong to an item
    /// after the block's dedent; the next `take_leads` returns them first.
    pending: Vec<Lead>,
    /// Parenthesis depth. Inside parentheses, line-structure tokens
    /// (`Newline`, `Indent`, `Dedent`) are skipped transparently so wrapped
    /// argument lists parse like single lines. Square brackets deliberately
    /// do not participate: an unclosed list must fail on its own line.
    bracket_depth: usize,
    /// `Indent`s skipped inside brackets whose matching `Dedent`s are owed;
    /// those dedents are consumed silently when they surface.
    suppressed: usize,
    eof_span: Span,
}

impl Stream {
    pub(super) fn new(tokens: Vec<Token>, source_len: usize, eof_line: usize) -> Self {
        Self {
            tokens,
            pos: 0,
            last_line: 0,
            pending: Vec::new(),
            bracket_depth: 0,
            suppressed: 0,
            eof_span: Span {
                start: source_len,
                end: source_len,
                line: eof_line,
                column: 1,
            },
        }
    }

    /// Span used for end-of-input diagnostics.
    pub(super) const fn eof_span(&self) -> Span {
        self.eof_span
    }

    /// Skip tokens that are structurally invisible at the current position:
    /// owed dedents from bracket-suppressed indents and, inside brackets,
    /// all line-structure tokens.
    fn settle(&mut self) {
        loop {
            let Some(token) = self.tokens.get(self.pos) else {
                return;
            };
            match token.kind {
                TokenKind::Dedent if self.suppressed > 0 => {
                    self.suppressed -= 1;
                    self.pos += 1;
                }
                TokenKind::Newline if self.bracket_depth > 0 => {
                    self.last_line = token.span.line;
                    self.pos += 1;
                }
                TokenKind::Indent if self.bracket_depth > 0 => {
                    self.suppressed += 1;
                    self.pos += 1;
                }
                _ => return,
            }
        }
    }

    pub(super) fn peek(&mut self) -> Option<&Token> {
        self.settle();
        self.tokens.get(self.pos)
    }

    /// The token after the next one, past structural noise at the current
    /// position (but not past noise in between).
    pub(super) fn peek_second(&mut self) -> Option<&Token> {
        self.settle();
        self.tokens.get(self.pos + 1)
    }

    pub(super) fn next(&mut self) -> Option<Token> {
        self.settle();
        let token = self.tokens.get(self.pos)?.clone();
        self.pos += 1;
        match token.kind {
            TokenKind::LeftParen => self.bracket_depth += 1,
            TokenKind::RightParen => {
                self.bracket_depth = self.bracket_depth.saturating_sub(1);
            }
            TokenKind::Indent | TokenKind::Dedent => return Some(token),
            _ => {}
        }
        self.last_line = token.span.line;
        Some(token)
    }

    /// Consume the next token if `test` accepts its kind.
    pub(super) fn eat(&mut self, test: impl Fn(&TokenKind) -> bool) -> Option<Token> {
        if self.peek().is_some_and(|token| test(&token.kind)) {
            self.next()
        } else {
            None
        }
    }

    pub(super) fn peek_is(&mut self, test: impl Fn(&TokenKind) -> bool) -> bool {
        self.peek().is_some_and(|token| test(&token.kind))
    }

    /// Span of the next token, or the end-of-input span.
    pub(super) fn peek_span(&mut self) -> Span {
        let fallback = self.eof_span;
        self.peek().map_or(fallback, |token| token.span)
    }

    /// Collect leading trivia — blank lines and own-line comments — up to
    /// the next significant token. Doc lines stop collection (callers that
    /// accept docs take them separately); a stray `//!` header line past the
    /// prologue is an error.
    pub(super) fn take_leads(&mut self) -> Result<Vec<Lead>, ParseError> {
        let mut leads = std::mem::take(&mut self.pending);
        loop {
            let last_line = self.last_line;
            let Some(token) = self.peek() else {
                return Ok(leads);
            };
            let span = token.span;
            match &token.kind {
                TokenKind::Comment(text) => {
                    let text = text.clone();
                    push_blank_if_gap(&mut leads, last_line, span.line);
                    leads.push(Lead::Comment(Comment { span, text }));
                    self.next();
                }
                TokenKind::DocHeader(_) => {
                    return Err(ParseError::new(
                        span,
                        "`//!` narration lines belong at the very top of the document, \
                         before the `workflow` header",
                    ));
                }
                _ => {
                    push_blank_if_gap(&mut leads, last_line, span.line);
                    return Ok(leads);
                }
            }
        }
    }

    /// Collect a contiguous run of `///` doc lines (each followed by its
    /// newline) attached to the declaration that follows.
    pub(super) fn take_docs(&mut self) -> Vec<DocLine> {
        let mut docs = Vec::new();
        while let Some(token) = self.peek() {
            let span = token.span;
            let TokenKind::DocLine(text) = &token.kind else {
                break;
            };
            let text = text.clone();
            self.next();
            docs.push(DocLine { span, text });
            self.eat(|kind| matches!(kind, TokenKind::Newline));
        }
        docs
    }

    /// Take a same-line trailing comment, if one precedes the line's newline.
    pub(super) fn take_trailing(&mut self) -> Option<Comment> {
        let token = self.peek()?;
        let span = token.span;
        if let TokenKind::Comment(text) = &token.kind {
            let text = text.clone();
            self.next();
            Some(Comment { span, text })
        } else {
            None
        }
    }

    /// Give collected leads back so a later item (possibly in an outer
    /// block) picks them up.
    pub(super) fn push_back_leads(&mut self, mut leads: Vec<Lead>) {
        leads.append(&mut self.pending);
        self.pending = leads;
    }

    /// Consume the end of the current line: an optional trailing comment
    /// followed by a newline (or end of input, or a block boundary).
    pub(super) fn end_line(&mut self) -> Result<Option<Comment>, ParseError> {
        let trailing = self.take_trailing();
        match self.peek() {
            None => Ok(trailing),
            Some(token) => match token.kind {
                TokenKind::Newline => {
                    self.next();
                    Ok(trailing)
                }
                TokenKind::Dedent => Ok(trailing),
                _ => Err(ParseError::new(
                    token.span,
                    format!("expected end of line, found {}", describe(&token.kind)),
                )),
            },
        }
    }

    /// Expect a block to open (an `Indent` token), attributing the error to
    /// `owner_span` with `expectation` when it does not.
    pub(super) fn expect_indent(
        &mut self,
        owner_span: Span,
        expectation: &str,
    ) -> Result<(), ParseError> {
        if self.eat(|kind| matches!(kind, TokenKind::Indent)).is_some() {
            Ok(())
        } else {
            Err(ParseError::new(owner_span, expectation.to_owned()))
        }
    }

    /// Whether the current block is over: the next token is a dedent or the
    /// input is exhausted.
    pub(super) fn at_block_end(&mut self) -> bool {
        self.peek()
            .is_none_or(|token| matches!(token.kind, TokenKind::Dedent))
    }

    /// Block end for item loops: a dedent/EOF, or a `///` run that attaches
    /// to a declaration in an OUTER block. Doc lines never adjust the
    /// lexer's indentation stack, so docs leading the next outer
    /// declaration sit before this block's dedent; consuming them here
    /// would attach them to the wrong owner.
    pub(super) fn at_item_block_end(&mut self) -> bool {
        if self.at_block_end() {
            return true;
        }
        if self.peek_is(|kind| matches!(kind, TokenKind::DocLine(_))) {
            return !self.docs_attach_here();
        }
        false
    }

    /// Whether a doc-line run at the cursor attaches inside the current
    /// block: false when the first significant token after the run is a
    /// dedent (or the input ends).
    fn docs_attach_here(&mut self) -> bool {
        self.settle();
        let mut index = self.pos;
        while let Some(token) = self.tokens.get(index) {
            match token.kind {
                TokenKind::DocLine(_) | TokenKind::Newline | TokenKind::Comment(_) => index += 1,
                TokenKind::Dedent => return false,
                _ => return true,
            }
        }
        false
    }

    /// Continue a wrapped pipe chain: when the line ends and the next line
    /// (one level deeper for the first continuation) starts with `|>`,
    /// consume the line structure so the chain parses as one statement. The
    /// continuation's eventual dedent is owed and swallowed silently.
    pub(super) fn continue_wrapped_pipe(&mut self) -> bool {
        self.settle();
        let mut index = self.pos;
        if !matches!(
            self.tokens.get(index).map(|token| &token.kind),
            Some(TokenKind::Newline)
        ) {
            return false;
        }
        index += 1;
        let indented = matches!(
            self.tokens.get(index).map(|token| &token.kind),
            Some(TokenKind::Indent)
        );
        if indented {
            index += 1;
        }
        if !matches!(
            self.tokens.get(index).map(|token| &token.kind),
            Some(TokenKind::Pipe)
        ) {
            return false;
        }
        self.next();
        if indented {
            self.next();
            self.suppressed += 1;
        }
        true
    }

    /// Open an indented block whose first line may be a `///` doc run: doc
    /// lines never adjust the lexer's indentation stack, so the `Indent`
    /// for the block's first code line can sit after the docs. Consumes
    /// that `Indent` wherever it sits in the leading trivia run and leaves
    /// the docs at the cursor for the first item to take; consumes nothing
    /// when no block opens.
    pub(super) fn open_block(&mut self) -> bool {
        self.settle();
        let mut index = self.pos;
        while let Some(token) = self.tokens.get(index) {
            match token.kind {
                TokenKind::Indent => {
                    self.tokens.remove(index);
                    return true;
                }
                TokenKind::DocLine(_) | TokenKind::Newline | TokenKind::Comment(_) => index += 1,
                _ => return false,
            }
        }
        false
    }

    /// Close an indented block: consume its `Dedent`, which may sit past a
    /// doc-line run that belongs to the next outer declaration (doc lines
    /// never adjust the indentation stack). The run itself is left in place
    /// for the outer context to consume in order.
    pub(super) fn consume_block_dedent(&mut self) {
        self.settle();
        let mut index = self.pos;
        while let Some(token) = self.tokens.get(index) {
            match token.kind {
                TokenKind::Dedent => {
                    self.tokens.remove(index);
                    return;
                }
                TokenKind::DocLine(_) | TokenKind::Newline | TokenKind::Comment(_) => index += 1,
                _ => return,
            }
        }
    }

    /// Expect and consume a specific simple token kind.
    pub(super) fn expect(&mut self, want: &TokenKind, message: &str) -> Result<Token, ParseError> {
        match self.peek() {
            Some(token) if token.kind == *want => {}
            Some(token) => return Err(ParseError::new(token.span, message.to_owned())),
            None => return Err(ParseError::new(self.eof_span, message.to_owned())),
        }
        self.next()
            .ok_or_else(|| ParseError::new(self.eof_span, message.to_owned()))
    }

    /// Expect an identifier-shaped token (either casing; the checker owns
    /// casing rules) used as a name; reserved keywords are refused with a
    /// diagnostic naming the keyword, while *contextual* keywords (the
    /// combinator, config, and predicate words) double as ordinary names.
    pub(super) fn expect_name(&mut self, what: &str) -> Result<(String, Span), ParseError> {
        match self.peek() {
            Some(token) => {
                let span = token.span;
                match &token.kind {
                    TokenKind::Identifier(name) | TokenKind::TypeIdentifier(name) => {
                        let name = name.clone();
                        self.next();
                        Ok((name, span))
                    }
                    TokenKind::Keyword(keyword) if soft_keyword(*keyword) => {
                        let name = keyword.as_word().to_owned();
                        self.next();
                        Ok((name, span))
                    }
                    TokenKind::Keyword(keyword) => Err(ParseError::new(
                        span,
                        format!(
                            "`{}` is a reserved keyword and cannot be used as {what}",
                            keyword.as_word()
                        ),
                    )),
                    other => Err(ParseError::new(
                        span,
                        format!("expected {what}, found {}", describe(other)),
                    )),
                }
            }
            None => Err(ParseError::new(
                self.eof_span,
                format!("expected {what}, found end of input"),
            )),
        }
    }
}

/// Contextual keywords: words that are keywords only in their own grammar
/// positions (combinator stages, config lines, `is` predicates) and act as
/// ordinary names everywhere a name is expected. The corpus pins `count` as
/// a field name and `retry` as an outcome-arm name; structural keywords
/// stay reserved everywhere (`input step:` is a parse error).
pub(super) fn soft_keyword(keyword: crate::Keyword) -> bool {
    use crate::Keyword as K;
    matches!(
        keyword,
        K::Filter
            | K::Map
            | K::Sort
            | K::Count
            | K::Node
            | K::Timeout
            | K::Retry
            | K::Every
            | K::Backoff
            | K::Empty
            | K::Present
            | K::Absent
    )
}

fn push_blank_if_gap(leads: &mut Vec<Lead>, last_line: usize, next_line: usize) {
    if last_line > 0 && next_line > last_line + 1 && !matches!(leads.last(), Some(Lead::Blank)) {
        leads.push(Lead::Blank);
    }
}

/// A short human description of a token kind for diagnostics.
pub(super) fn describe(kind: &TokenKind) -> String {
    match kind {
        TokenKind::Keyword(keyword) => format!("keyword `{}`", keyword.as_word()),
        TokenKind::Identifier(name) | TokenKind::TypeIdentifier(name) => format!("`{name}`"),
        TokenKind::FieldAccessor(name) => format!("`.{name}`"),
        TokenKind::String(_) => "a string literal".to_owned(),
        TokenKind::Integer(value) => format!("`{value}`"),
        TokenKind::Float(text) => format!("`{text}`"),
        TokenKind::Duration { .. } => "a duration literal".to_owned(),
        TokenKind::LeftParen => "`(`".to_owned(),
        TokenKind::RightParen => "`)`".to_owned(),
        TokenKind::LeftBrace => "`{`".to_owned(),
        TokenKind::RightBrace => "`}`".to_owned(),
        TokenKind::LeftBracket => "`[`".to_owned(),
        TokenKind::RightBracket => "`]`".to_owned(),
        TokenKind::Colon => "`:`".to_owned(),
        TokenKind::Comma => "`,`".to_owned(),
        TokenKind::Arrow => "`->`".to_owned(),
        TokenKind::Pipe => "`|>`".to_owned(),
        TokenKind::Bar => "`|`".to_owned(),
        TokenKind::Question => "`?`".to_owned(),
        TokenKind::Equal => "`=`".to_owned(),
        TokenKind::DotDot => "`..`".to_owned(),
        TokenKind::Plus => "`+`".to_owned(),
        TokenKind::EqualEqual => "`==`".to_owned(),
        TokenKind::BangEqual => "`!=`".to_owned(),
        TokenKind::Less => "`<`".to_owned(),
        TokenKind::LessEqual => "`<=`".to_owned(),
        TokenKind::Greater => "`>`".to_owned(),
        TokenKind::GreaterEqual => "`>=`".to_owned(),
        TokenKind::Newline => "end of line".to_owned(),
        TokenKind::DocHeader(_) => "a `//!` narration line".to_owned(),
        TokenKind::DocLine(_) => "a `///` doc line".to_owned(),
        TokenKind::Comment(_) => "a comment".to_owned(),
        TokenKind::SchemaBody(_) => "an inline schema body".to_owned(),
        TokenKind::Indent => "an indented block".to_owned(),
        TokenKind::Dedent => "end of an indented block".to_owned(),
    }
}

/// Migration fix-its for AWL-0/1 keywords that are gone in rev-2. The
/// message names both the dead word and its rev-2 replacement.
pub(super) fn gone_keyword_hint(word: &str) -> Option<String> {
    let hint = match word {
        "about" => {
            "rev-2 has no `about`: prose is doc comments — `///` on declarations, `//!` narration at the top"
        }
        "do" => {
            "rev-2 has no `do`: call the action directly and bind with `->` — `action_name(arg: value) -> name`"
        }
        "as" => {
            "rev-2 has no `as`: bind a call's result with `->` — `action_name(arg: value) -> name`"
        }
        "each" => "rev-2 has no `each`: fan out with `fork item in items … join -> name`",
        "repeat" | "up" => {
            "rev-2 has no `repeat`/`up to`: iterate with `loop <name> = <seed> … until <cond> … max <bound>`"
        }
        "finish" => "rev-2 has no `finish`: finishing IS routing — `route <workflow outcome>`",
        "fail" => {
            "rev-2 has no `fail`: route to a failure-mapped outcome — `route <workflow outcome>`"
        }
        "match" | "case" => {
            "rev-2 has no `match`/`case`: branch with outcome clauses — `outcome <name>: when <cond>, route <target>`"
        }
        "parallel" => {
            "rev-2 has no `parallel`: use `fork … join`, or independent steps sharing an `after` dependency"
        }
        "race" => {
            "rev-2 has no `race`: `wait <signal> timeout <duration>` covers signal-or-deadline"
        }
        "output" => "rev-2 has no `output`: declare `outcome <name>: type <Type>, route success`",
        "error" => "rev-2 has no `error`: declare `outcome <name>: type <Type>, route failure`",
        "queue" => {
            "rev-2 has no `queue`: the worker name is the task queue — declare the action in a `worker` block"
        }
        _ => return None,
    };
    Some(hint.to_owned())
}

/// Migration fix-its for AWL-0/1 type constructors that are gone in rev-2.
pub(super) fn gone_type_hint(name: &str) -> Option<String> {
    match name {
        "Option" => {
            Some("rev-2 has no `Option(T)`: optionality is postfix — write `T?`".to_owned())
        }
        "List" => Some("rev-2 has no `List(T)`: the one list spelling is `[T]`".to_owned()),
        _ => None,
    }
}
