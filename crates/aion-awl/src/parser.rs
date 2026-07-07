#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::unnecessary_wraps,
    clippy::unwrap_used
)]

use std::error::Error;
use std::fmt;

use crate::ast::{
    AboutDecl, ActionDecl, ActionFieldTag, BinaryOp, BindDecl, CallExpr, CallTarget, Comment,
    Document, DurationLiteral, EachSpec, Expr, FieldDecl, HandlerBlock, HandlerTerminal, IoDecl,
    RecordField, RetrySpec, Spanned, StepDecl, StepFieldTag, StepOp, Trivia, TypeDecl, TypeRef,
    WorkflowDecl, join_span,
};
use crate::{DurationUnit, Keyword, LexError, Span, Token, TokenKind, lex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub span: Span,
    pub message: String,
}

impl ParseError {
    fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} at line {}, column {}",
            self.message, self.span.line, self.span.column
        )
    }
}

impl Error for ParseError {}

impl From<LexError> for ParseError {
    fn from(value: LexError) -> Self {
        Self::new(value.span, value.message)
    }
}

/// Parse an AWL source document into a spanned [`Document`].
///
/// The parser reports the first lexical or syntactic error it encounters.
///
/// # Errors
///
/// Returns [`ParseError`] when the source is not valid AWL-0 or cannot be lexed.
pub fn parse(source: &str) -> Result<Document, ParseError> {
    let lines = SourceLines::new(source)?;
    LineParser::new(lines).parse_document()
}

#[derive(Debug, Clone)]
struct SourceLine {
    indent: usize,
    code: String,
    trailing: Option<Comment>,
    span: Span,
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
    fn fragment_span(&self, fragment: &str) -> Span {
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

struct SourceLines {
    lines: Vec<SourceLine>,
    comments: Vec<Comment>,
}

impl SourceLines {
    fn new(source: &str) -> Result<Self, ParseError> {
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
fn span(start: usize, end: usize, line: usize, column: usize) -> Span {
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
fn lex_at(text: &str, base: Span) -> Result<Vec<Token>, LexError> {
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
fn rebase_span(fragment_relative: Span, base: Span) -> Span {
    span(
        base.start + fragment_relative.start,
        base.start + fragment_relative.end,
        base.line,
        base.column + fragment_relative.column - 1,
    )
}

/// The canonical top-level declaration order: `workflow`, `about`, `input*`,
/// `output`, `error?`, `signal*`, `type*`, `action*`, `step*`, `finish`.
/// `workflow`/`about` are parsed unconditionally before the phase loop
/// begins, and `finish` is checked separately as the mandatory final line,
/// so this only orders the middle group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DeclPhase {
    Input,
    Output,
    Error,
    Signal,
    Type,
    Action,
    Step,
}

impl DeclPhase {
    fn of(keyword: &str) -> Option<Self> {
        match keyword {
            "input" => Some(Self::Input),
            "output" => Some(Self::Output),
            "error" => Some(Self::Error),
            "signal" => Some(Self::Signal),
            "type" => Some(Self::Type),
            "action" => Some(Self::Action),
            "step" => Some(Self::Step),
            _ => None,
        }
    }

    /// Whether more than one declaration may occupy this phase (`input`,
    /// `signal`, `type`, `action`, `step` are repeatable; `output` and
    /// `error` admit at most one).
    const fn repeatable(self) -> bool {
        !matches!(self, Self::Output | Self::Error)
    }

    /// Human-readable list of declarations still valid from this phase
    /// onward, for "out of order" diagnostics.
    const fn expected_text(self) -> &'static str {
        match self {
            Self::Input => {
                "`input`, `output`, `error`, `signal`, `type`, `action`, `step`, or `finish`"
            }
            Self::Output => "`output`, `error`, `signal`, `type`, `action`, `step`, or `finish`",
            Self::Error => "`error`, `signal`, `type`, `action`, `step`, or `finish`",
            Self::Signal => "`signal`, `type`, `action`, `step`, or `finish`",
            Self::Type => "`type`, `action`, `step`, or `finish`",
            Self::Action => "`action`, `step`, or `finish`",
            Self::Step => "`step` or `finish`",
        }
    }
}

struct LineParser {
    lines: Vec<SourceLine>,
    own_comments: Vec<Comment>,
    pos: usize,
    comment_pos: usize,
}

impl LineParser {
    fn new(source: SourceLines) -> Self {
        Self {
            lines: source.lines,
            own_comments: source.comments,
            pos: 0,
            comment_pos: 0,
        }
    }

    fn parse_document(mut self) -> Result<Document, ParseError> {
        let workflow = self.parse_workflow()?;
        let about = self.parse_maybe_about(0)?;
        let mut inputs = Vec::new();
        let mut output = None;
        let mut error = None;
        let mut signals = Vec::new();
        let mut types = Vec::new();
        let mut actions = Vec::new();
        let mut steps = Vec::new();
        let mut finish = None;
        let mut finish_leading = Vec::new();
        let mut phase = DeclPhase::Input;
        while let Some(line) = self.peek() {
            if line.indent != 0 {
                return Err(ParseError::new(
                    line.span,
                    "top-level declarations must start at column 1",
                ));
            }
            let first = first_word(&line.code);
            if first != "finish" {
                let this_phase = DeclPhase::of(first).ok_or_else(|| {
                    ParseError::new(line.span, format!("unknown declaration `{first}`"))
                })?;
                if this_phase < phase {
                    return Err(ParseError::new(
                        line.span,
                        format!(
                            "`{first}` is out of canonical order; expected {} here (canonical order is workflow, about, input, output, error, signal, type, action, step, finish)",
                            phase.expected_text()
                        ),
                    ));
                }
                if this_phase == phase && !this_phase.repeatable() {
                    return Err(ParseError::new(
                        line.span,
                        format!("duplicate `{first}` declaration; only one is allowed"),
                    ));
                }
                phase = this_phase;
            }
            match first {
                "input" => inputs.push(self.parse_io("input")?),
                "output" => {
                    output = Some(self.parse_io("output")?);
                }
                "error" => {
                    error = Some(self.parse_io("error")?);
                }
                "signal" => signals.push(self.parse_io("signal")?),
                "type" => types.push(self.parse_type_decl()?),
                "action" => actions.push(self.parse_action_decl()?),
                "step" => steps.push(self.parse_step()?),
                "finish" => {
                    let line = self.bump().expect("peeked");
                    finish_leading = self.take_trivia(&line).leading;
                    let rest = line.code.strip_prefix("finish").unwrap().trim_start();
                    finish = Some(parse_expr_at(&line, rest)?);
                    if self.peek().is_some() {
                        return Err(ParseError::new(
                            line.span,
                            "finish must be the final declaration",
                        ));
                    }
                }
                _ => unreachable!("unknown declarations are rejected above"),
            }
        }
        let finish = finish.ok_or_else(|| {
            let end = self.lines.last().map_or(workflow.span, |line| line.span);
            ParseError::new(end, "missing finish declaration at document end")
        })?;
        let span = join_span(workflow.span, finish.span());
        let mut comments = self.own_comments.clone();
        for line in &self.lines {
            if let Some(comment) = &line.trailing {
                comments.push(comment.clone());
            }
        }
        Ok(Document {
            span,
            workflow,
            about,
            inputs,
            output,
            error,
            signals,
            types,
            actions,
            steps,
            finish,
            finish_leading,
            comments,
        })
    }

    fn parse_workflow(&mut self) -> Result<WorkflowDecl, ParseError> {
        let line = self.bump_required("missing workflow declaration")?;
        let trivia = self.take_trivia(&line);
        let name = line
            .code
            .strip_prefix("workflow ")
            .ok_or_else(|| {
                ParseError::new(line.span, "document must start with workflow declaration")
            })?
            .trim();
        Ok(WorkflowDecl {
            span: line.span,
            trivia,
            name: name.to_owned(),
        })
    }

    fn parse_maybe_about(&mut self, indent: usize) -> Result<Option<AboutDecl>, ParseError> {
        if self
            .peek()
            .is_some_and(|line| line.indent == indent && first_word(&line.code) == "about")
        {
            let line = self.bump().expect("peeked");
            let trivia = self.take_trivia(&line);
            let text = line
                .code
                .strip_prefix("about")
                .unwrap()
                .trim_start()
                .to_owned();
            Ok(Some(AboutDecl {
                span: line.span,
                trivia,
                text,
            }))
        } else {
            Ok(None)
        }
    }

    fn parse_io(&mut self, keyword: &str) -> Result<IoDecl, ParseError> {
        let line = self.bump().expect("peeked");
        let trivia = self.take_trivia(&line);
        let rest = line.code.strip_prefix(keyword).unwrap().trim_start();
        let (name, ty_text) = if matches!(keyword, "output" | "error") && !rest.contains(':') {
            ("", rest)
        } else {
            rest.split_once(':').ok_or_else(|| {
                ParseError::new(
                    line.span,
                    format!("{keyword} declaration needs `name: Type`"),
                )
            })?
        };
        Ok(IoDecl {
            span: line.span,
            trivia,
            name: name.trim().to_owned(),
            ty: parse_type_at(&line, ty_text.trim())?,
        })
    }

    fn parse_type_decl(&mut self) -> Result<TypeDecl, ParseError> {
        let line = self.bump().expect("peeked");
        let trivia = self.take_trivia(&line);
        let rest = line.code.strip_prefix("type").unwrap().trim_start();
        let (name, body) = rest
            .split_once('{')
            .ok_or_else(|| ParseError::new(line.span, "type declaration needs record fields"))?;
        let body = body
            .strip_suffix('}')
            .ok_or_else(|| ParseError::new(line.span, "unterminated type record"))?;
        let mut fields = Vec::new();
        for part in comma_parts(body) {
            if part.trim().is_empty() {
                continue;
            }
            let (field, ty) = part
                .split_once(':')
                .ok_or_else(|| ParseError::new(line.span, "type field needs `name: Type`"))?;
            fields.push(FieldDecl {
                span: line.span,
                name: field.trim().to_owned(),
                ty: parse_type_at(&line, ty.trim())?,
            });
        }
        Ok(TypeDecl {
            span: line.span,
            trivia,
            name: name.trim().to_owned(),
            fields,
        })
    }

    fn parse_action_decl(&mut self) -> Result<ActionDecl, ParseError> {
        let line = self.bump().expect("peeked");
        let trivia = self.take_trivia(&line);
        let sig = line.code.strip_prefix("action").unwrap().trim_start();
        let (left, ret) = sig.split_once("->").ok_or_else(|| {
            ParseError::new(line.span, "action declaration needs `-> ReturnType`")
        })?;
        let open = left
            .find('(')
            .ok_or_else(|| ParseError::new(line.span, "action declaration needs parameter list"))?;
        let close = left
            .rfind(')')
            .ok_or_else(|| ParseError::new(line.span, "action declaration needs closing `)`"))?;
        let name = left[..open].trim().to_owned();
        let mut params = Vec::new();
        for part in comma_parts(&left[open + 1..close]) {
            if part.trim().is_empty() {
                continue;
            }
            let (param, ty) = part
                .split_once(':')
                .ok_or_else(|| ParseError::new(line.span, "action parameter needs `name: Type`"))?;
            params.push(FieldDecl {
                span: line.span,
                name: param.trim().to_owned(),
                ty: parse_type_at(&line, ty.trim())?,
            });
        }
        let returns = parse_type_at(&line, ret.trim())?;
        let mut action = ActionDecl {
            span: line.span,
            trivia,
            name,
            params,
            returns,
            queue: None,
            node: None,
            timeout: None,
            retry: None,
            leading_comments: Vec::new(),
        };
        if self.peek().is_some_and(|next| next.indent == 2) {
            while self.peek().is_some_and(|next| next.indent == 2) {
                let field = self.bump().expect("peeked");
                let field_trivia = self.take_trivia(&field);
                let tag = match first_word(&field.code) {
                    "queue" => {
                        action.queue = Some(parse_string_field(&field, "queue")?);
                        ActionFieldTag::Queue
                    }
                    "node" => {
                        action.node = Some(parse_string_field(&field, "node")?);
                        ActionFieldTag::Node
                    }
                    "timeout" => {
                        action.timeout = Some(parse_duration_field(&field, "timeout")?);
                        ActionFieldTag::Timeout
                    }
                    "retry" => {
                        action.retry = Some(parse_retry(&field)?);
                        ActionFieldTag::Retry
                    }
                    other => {
                        return Err(ParseError::new(
                            field.span,
                            format!("unknown action field `{other}`"),
                        ));
                    }
                };
                if !field_trivia.leading.is_empty() {
                    action.leading_comments.push((tag, field_trivia.leading));
                }
            }
        }
        Ok(action)
    }

    fn parse_step(&mut self) -> Result<StepDecl, ParseError> {
        let head = self.bump().expect("peeked");
        let trivia = self.take_trivia(&head);
        let name = head
            .code
            .strip_prefix("step ")
            .ok_or_else(|| ParseError::new(head.span, "step declaration needs a name"))?
            .trim()
            .to_owned();
        let mut about = None;
        let mut when = None;
        let mut each = None;
        let mut op: Option<StepOp> = None;
        let mut repeat = None;
        let mut until = None;
        let mut retry = None;
        let mut timeout = None;
        let mut on_timeout = None;
        let mut on_failure = None;
        let mut bind_as = None;
        let mut queue = None;
        let mut node = None;
        let mut leading_comments: Vec<(StepFieldTag, Vec<Comment>)> = Vec::new();
        while self.peek().is_some_and(|line| line.indent == 2) {
            let line = self.bump().expect("peeked");
            match first_word(&line.code) {
                "about" => {
                    about = Some(AboutDecl {
                        span: line.span,
                        trivia: self.take_trivia(&line),
                        text: line
                            .code
                            .strip_prefix("about")
                            .unwrap()
                            .trim_start()
                            .to_owned(),
                    });
                }
                "when" => {
                    let trivia = self.take_trivia(&line);
                    push_leading(&mut leading_comments, StepFieldTag::When, trivia.leading);
                    when = Some(parse_expr_at(
                        &line,
                        line.code.strip_prefix("when").unwrap().trim_start(),
                    )?);
                }
                "each" => {
                    let trivia = self.take_trivia(&line);
                    push_leading(&mut leading_comments, StepFieldTag::Each, trivia.leading);
                    each = Some(parse_each(&line)?);
                }
                "do" => {
                    let trivia = self.take_trivia(&line);
                    push_leading(&mut leading_comments, StepFieldTag::Op, trivia.leading);
                    set_op(&mut op, StepOp::Do(parse_do(&line)?), line.span)?;
                }
                "wait" => {
                    let trivia = self.take_trivia(&line);
                    push_leading(&mut leading_comments, StepFieldTag::Op, trivia.leading);
                    set_op(
                        &mut op,
                        StepOp::Wait {
                            span: line.span,
                            signal: line.code.strip_prefix("wait").unwrap().trim().to_owned(),
                        },
                        line.span,
                    )?;
                }
                "sleep" => {
                    let trivia = self.take_trivia(&line);
                    push_leading(&mut leading_comments, StepFieldTag::Op, trivia.leading);
                    set_op(
                        &mut op,
                        StepOp::Sleep(parse_duration_field(&line, "sleep")?),
                        line.span,
                    )?;
                }
                "repeat" => {
                    let trivia = self.take_trivia(&line);
                    push_leading(&mut leading_comments, StepFieldTag::Repeat, trivia.leading);
                    repeat = Some(parse_repeat(&line)?);
                }
                "until" => {
                    let trivia = self.take_trivia(&line);
                    push_leading(&mut leading_comments, StepFieldTag::Until, trivia.leading);
                    until = Some(parse_expr_at(
                        &line,
                        line.code.strip_prefix("until").unwrap().trim_start(),
                    )?);
                }
                "retry" => {
                    let trivia = self.take_trivia(&line);
                    push_leading(&mut leading_comments, StepFieldTag::Retry, trivia.leading);
                    retry = Some(parse_retry(&line)?);
                }
                "timeout" => {
                    let trivia = self.take_trivia(&line);
                    push_leading(&mut leading_comments, StepFieldTag::Timeout, trivia.leading);
                    timeout = Some(parse_duration_field(&line, "timeout")?);
                }
                "on" if line.code == "on timeout" => {
                    let trivia = self.take_trivia(&line);
                    push_leading(
                        &mut leading_comments,
                        StepFieldTag::OnTimeout,
                        trivia.leading,
                    );
                    on_timeout = Some(self.parse_handler(line.span)?);
                }
                "on" if line.code == "on failure" => {
                    let trivia = self.take_trivia(&line);
                    push_leading(
                        &mut leading_comments,
                        StepFieldTag::OnFailure,
                        trivia.leading,
                    );
                    on_failure = Some(self.parse_handler(line.span)?);
                }
                "as" => {
                    bind_as = Some(BindDecl {
                        span: line.span,
                        trivia: self.take_trivia(&line),
                        name: line.code.strip_prefix("as").unwrap().trim().to_owned(),
                    });
                }
                "queue" => {
                    let trivia = self.take_trivia(&line);
                    push_leading(&mut leading_comments, StepFieldTag::Queue, trivia.leading);
                    queue = Some(parse_string_field(&line, "queue")?);
                }
                "node" => {
                    let trivia = self.take_trivia(&line);
                    push_leading(&mut leading_comments, StepFieldTag::Node, trivia.leading);
                    node = Some(parse_string_field(&line, "node")?);
                }
                other => {
                    return Err(ParseError::new(
                        line.span,
                        format!("unknown step field `{other}`"),
                    ));
                }
            }
        }
        if self
            .peek()
            .is_some_and(|line| line.indent > 0 && line.indent != 2)
        {
            let line = self.peek().unwrap();
            return Err(ParseError::new(
                line.span,
                "wrong indentation depth for step field or handler block body",
            ));
        }
        let op = op.ok_or_else(|| {
            ParseError::new(
                head.span,
                "step must contain exactly one of do, wait, or sleep",
            )
        })?;
        let end = self
            .lines
            .get(self.pos.saturating_sub(1))
            .map_or(head.span, |line| line.span);
        Ok(StepDecl {
            span: join_span(head.span, end),
            trivia,
            name,
            about,
            when,
            each,
            op,
            repeat,
            until,
            retry,
            timeout,
            on_timeout,
            on_failure,
            bind_as,
            queue,
            node,
            leading_comments,
        })
    }

    fn parse_handler(&mut self, head: Span) -> Result<HandlerBlock, ParseError> {
        if self.peek().is_none_or(|line| line.indent != 4) {
            let err = self.peek().map_or(head, |line| line.span);
            return Err(ParseError::new(
                err,
                "wrong indentation depth for a handler block body",
            ));
        }
        let mut actions = Vec::new();
        let mut action_leading = Vec::new();
        let mut terminal: Option<HandlerTerminal> = None;
        let mut terminal_leading = Vec::new();
        while self.peek().is_some_and(|line| line.indent == 4) {
            let line = self.bump().expect("peeked");
            let trivia = self.take_trivia(&line);
            match first_word(&line.code) {
                "do" => {
                    if terminal.is_some() {
                        return Err(ParseError::new(
                            line.span,
                            "handler block `do` line must come before the terminal (`finish`/`fail` must be last)",
                        ));
                    }
                    action_leading.push(trivia.leading);
                    actions.push(parse_do(&line)?);
                }
                "finish" => {
                    if terminal.is_some() {
                        return Err(ParseError::new(
                            line.span,
                            "handler block must have exactly one terminal (`finish` or `fail`)",
                        ));
                    }
                    terminal_leading = trivia.leading;
                    terminal = Some(HandlerTerminal::Finish(parse_expr_at(
                        &line,
                        line.code.strip_prefix("finish").unwrap().trim_start(),
                    )?));
                }
                "fail" if line.code == "fail" => {
                    if terminal.is_some() {
                        return Err(ParseError::new(
                            line.span,
                            "handler block must have exactly one terminal (`finish` or `fail`)",
                        ));
                    }
                    terminal_leading = trivia.leading;
                    terminal = Some(HandlerTerminal::Fail(line.span));
                }
                other => {
                    return Err(ParseError::new(
                        line.span,
                        format!("unknown handler field `{other}`"),
                    ));
                }
            }
        }
        let terminal =
            terminal.ok_or_else(|| ParseError::new(head, "handler block must finish or fail"))?;
        let end = self
            .lines
            .get(self.pos.saturating_sub(1))
            .map_or(head, |line| line.span);
        Ok(HandlerBlock {
            span: join_span(head, end),
            actions,
            action_leading,
            terminal,
            terminal_leading,
        })
    }

    fn peek(&self) -> Option<&SourceLine> {
        self.lines.get(self.pos)
    }
    fn bump(&mut self) -> Option<SourceLine> {
        let line = self.lines.get(self.pos).cloned();
        self.pos += usize::from(line.is_some());
        line
    }
    fn bump_required(&mut self, msg: &str) -> Result<SourceLine, ParseError> {
        self.bump()
            .ok_or_else(|| ParseError::new(span(0, 0, 1, 1), msg))
    }

    fn take_trivia(&mut self, line: &SourceLine) -> Trivia {
        let mut leading = Vec::new();
        while self.comment_pos < self.own_comments.len()
            && self.own_comments[self.comment_pos].span.start < line.span.start
        {
            leading.push(self.own_comments[self.comment_pos].clone());
            self.comment_pos += 1;
        }
        Trivia {
            leading,
            trailing: line.trailing.clone(),
        }
    }
}

fn first_word(code: &str) -> &str {
    code.split_whitespace().next().unwrap_or("")
}

fn parse_each(line: &SourceLine) -> Result<EachSpec, ParseError> {
    let rest = line.code.strip_prefix("each").unwrap().trim_start();
    let (name, expr) = rest
        .split_once(" in ")
        .ok_or_else(|| ParseError::new(line.span, "each field needs `name in expr`"))?;
    Ok(EachSpec {
        span: line.span,
        name: name.trim().to_owned(),
        in_expr: parse_expr_at(line, expr.trim())?,
    })
}

fn parse_repeat(line: &SourceLine) -> Result<Expr, ParseError> {
    let rest = line.code.strip_prefix("repeat").unwrap().trim_start();
    let expr = rest
        .strip_prefix("up to")
        .ok_or_else(|| ParseError::new(line.span, "repeat field needs `up to expr`"))?
        .trim_start();
    parse_expr_at(line, expr)
}

fn parse_string_field(line: &SourceLine, keyword: &str) -> Result<String, ParseError> {
    let rest = line.code.strip_prefix(keyword).unwrap().trim();
    let base = line.fragment_span(rest);
    let tokens = lex_at(rest, base)?;
    match tokens.as_slice() {
        [
            Token {
                kind: TokenKind::String(value),
                ..
            },
        ] => Ok(value.clone()),
        _ => Err(ParseError::new(
            base,
            format!("{keyword} field needs a string literal"),
        )),
    }
}

fn parse_duration_field(line: &SourceLine, keyword: &str) -> Result<DurationLiteral, ParseError> {
    let rest = line.code.strip_prefix(keyword).unwrap().trim();
    parse_duration_text(line, rest)
}

fn parse_duration_text(line: &SourceLine, text: &str) -> Result<DurationLiteral, ParseError> {
    let base = line.fragment_span(text);
    let tokens = lex_at(text, base)?;
    match tokens.as_slice() {
        [
            Token {
                kind: TokenKind::Duration { magnitude, unit },
                span,
            },
        ] => Ok(DurationLiteral {
            span: *span,
            magnitude: *magnitude,
            unit: *unit,
        }),
        _ => Err(ParseError::new(base, "expected duration literal")),
    }
}

fn parse_retry(line: &SourceLine) -> Result<RetrySpec, ParseError> {
    let tokens = lex_at(line.code.as_str(), line.span)?;
    match tokens.as_slice() {
        [
            Token {
                kind: TokenKind::Keyword(Keyword::Retry),
                ..
            },
            Token {
                kind: TokenKind::Integer(count),
                ..
            },
            Token {
                kind: TokenKind::Keyword(Keyword::Every),
                ..
            },
            Token {
                kind: TokenKind::Duration { magnitude, unit },
                span,
            },
        ] => Ok(RetrySpec::Every {
            span: line.span,
            count: *count,
            every: DurationLiteral {
                span: *span,
                magnitude: *magnitude,
                unit: *unit,
            },
        }),
        [
            Token {
                kind: TokenKind::Keyword(Keyword::Retry),
                ..
            },
            Token {
                kind: TokenKind::Integer(count),
                ..
            },
            Token {
                kind: TokenKind::Keyword(Keyword::Backoff),
                ..
            },
            Token {
                kind:
                    TokenKind::Duration {
                        magnitude: min_mag,
                        unit: min_unit,
                    },
                span: min_span,
            },
            Token {
                kind: TokenKind::DotDot,
                ..
            },
            Token {
                kind:
                    TokenKind::Duration {
                        magnitude: max_mag,
                        unit: max_unit,
                    },
                span: max_span,
            },
        ] => Ok(RetrySpec::Backoff {
            span: line.span,
            count: *count,
            min: DurationLiteral {
                span: *min_span,
                magnitude: *min_mag,
                unit: *min_unit,
            },
            max: DurationLiteral {
                span: *max_span,
                magnitude: *max_mag,
                unit: *max_unit,
            },
        }),
        _ => Err(ParseError::new(
            line.span,
            "retry field needs `retry n every d` or `retry n backoff d..d`",
        )),
    }
}

fn set_op(op: &mut Option<StepOp>, new_op: StepOp, span: Span) -> Result<(), ParseError> {
    if op.is_some() {
        return Err(ParseError::new(
            span,
            "step must contain exactly one of do, wait, or sleep",
        ));
    }
    *op = Some(new_op);
    Ok(())
}

/// Record a run of own-line comments as leading trivia for `tag`, if any
/// were found immediately before the field's line.
fn push_leading(
    leading_comments: &mut Vec<(StepFieldTag, Vec<Comment>)>,
    tag: StepFieldTag,
    leading: Vec<Comment>,
) {
    if !leading.is_empty() {
        leading_comments.push((tag, leading));
    }
}

fn parse_do(line: &SourceLine) -> Result<CallTarget, ParseError> {
    let rest = line.code.strip_prefix("do").unwrap().trim_start();
    if let Some(child) = rest.strip_prefix("child ") {
        let call = parse_call_text(line, child)?;
        Ok(CallTarget::Child {
            span: call.span,
            workflow: call.name,
            args: call.args,
        })
    } else {
        Ok(CallTarget::Action(parse_call_text(line, rest)?))
    }
}

fn parse_call_text(line: &SourceLine, text: &str) -> Result<CallExpr, ParseError> {
    let base = line.fragment_span(text);
    let open = text
        .find('(')
        .ok_or_else(|| ParseError::new(base, "do target must be a call"))?;
    let close = text
        .rfind(')')
        .ok_or_else(|| ParseError::new(base, "do target call needs closing `)`"))?;
    if text[close + 1..].trim().is_empty() {
        let mut args = Vec::new();
        for part in comma_parts(&text[open + 1..close]) {
            if part.trim().is_empty() {
                continue;
            }
            args.push(parse_expr_at(line, part.trim())?);
        }
        Ok(CallExpr {
            span: base,
            name: text[..open].trim().to_owned(),
            args,
        })
    } else {
        Err(ParseError::new(base, "unexpected text after call"))
    }
}

fn comma_parts(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0_i32;
    let mut in_string = false;
    let mut escaped = false;
    for (i, ch) in text.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else {
            match ch {
                '"' => in_string = true,
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => {
                    parts.push(&text[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
    }
    parts.push(&text[start..]);
    parts
}

fn parse_type_at(line: &SourceLine, text: &str) -> Result<TypeRef, ParseError> {
    let context = line.fragment_span(text);
    let tokens = lex_at(text, context)?;
    let mut parser = TypeParser {
        tokens: &tokens,
        pos: 0,
        context,
    };
    let ty = parser.parse_type()?;
    if parser.pos != tokens.len() {
        return Err(ParseError::new(
            tokens[parser.pos].span,
            "unexpected token in type",
        ));
    }
    Ok(ty)
}

struct TypeParser<'a> {
    tokens: &'a [Token],
    pos: usize,
    context: Span,
}
impl TypeParser<'_> {
    fn parse_type(&mut self) -> Result<TypeRef, ParseError> {
        let token = self
            .bump()
            .ok_or_else(|| ParseError::new(self.context, "expected type"))?;
        let token_span = token.span;
        match token.kind {
            TokenKind::TypeIdentifier(name) => {
                if (name == "List" || name == "Option") && self.eat(&TokenKind::LeftParen) {
                    let inner = self.parse_type()?;
                    self.expect(&TokenKind::RightParen, "expected `)` after type parameter")?;
                    if name == "List" {
                        Ok(TypeRef::List {
                            span: token_span,
                            inner: Box::new(inner),
                        })
                    } else {
                        Ok(TypeRef::Option {
                            span: token_span,
                            inner: Box::new(inner),
                        })
                    }
                } else {
                    Ok(TypeRef::Named {
                        span: token_span,
                        name,
                    })
                }
            }
            _ => Err(ParseError::new(token_span, "expected type name")),
        }
    }
    fn bump(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.pos).cloned();
        self.pos += usize::from(token.is_some());
        token
    }
    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.tokens.get(self.pos).is_some_and(|t| &t.kind == kind) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, kind: &TokenKind, msg: &str) -> Result<(), ParseError> {
        if self.eat(kind) {
            Ok(())
        } else {
            Err(ParseError::new(
                self.tokens.get(self.pos).map_or(self.context, |t| t.span),
                msg,
            ))
        }
    }
}

fn parse_expr_at(line: &SourceLine, text: &str) -> Result<Expr, ParseError> {
    let context = line.fragment_span(text);
    let tokens = lex_at(text, context)?;
    let mut parser = ExprParser {
        tokens: &tokens,
        pos: 0,
        context,
    };
    let expr = parser.parse_or()?;
    if parser.pos != tokens.len() {
        return Err(ParseError::new(
            tokens[parser.pos].span,
            "unexpected token in expression",
        ));
    }
    Ok(expr)
}

struct ExprParser<'a> {
    tokens: &'a [Token],
    pos: usize,
    context: Span,
}
impl ExprParser<'_> {
    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        self.parse_binary(Self::parse_and, &[Keyword::Or], &[BinaryOp::Or])
    }
    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        self.parse_binary(Self::parse_compare, &[Keyword::And], &[BinaryOp::And])
    }
    fn parse_compare(&mut self) -> Result<Expr, ParseError> {
        let left = self.parse_add()?;
        let Some(op) = self.comparison_op() else {
            return Ok(left);
        };
        let op_span = self.bump().expect("peeked").span;
        let right = self.parse_add()?;
        if self.comparison_op().is_some() {
            return Err(ParseError::new(op_span, "comparisons are non-associative"));
        }
        let span = join_span(left.span(), right.span());
        Ok(Expr::Binary {
            span,
            left: Box::new(left),
            op,
            right: Box::new(right),
        })
    }
    fn parse_add(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_not()?;
        while self.eat(&TokenKind::Plus) {
            let right = self.parse_not()?;
            let span = join_span(expr.span(), right.span());
            expr = Expr::Binary {
                span,
                left: Box::new(expr),
                op: BinaryOp::Add,
                right: Box::new(right),
            };
        }
        Ok(expr)
    }
    fn parse_not(&mut self) -> Result<Expr, ParseError> {
        if self.eat_keyword(Keyword::Not) {
            let expr = self.parse_not()?;
            let span = expr.span();
            Ok(Expr::Not {
                span,
                expr: Box::new(expr),
            })
        } else {
            self.parse_postfix()
        }
    }
    fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_primary()?;
        while self.eat(&TokenKind::Dot) {
            let token = self
                .bump()
                .ok_or_else(|| ParseError::new(self.context, "expected field name after `.`"))?;
            let field = match &token.kind {
                TokenKind::Identifier(name) => name.clone(),
                _ => return Err(ParseError::new(token.span, "expected field name after `.`")),
            };
            let span = join_span(expr.span(), token.span);
            expr = Expr::Field {
                span,
                base: Box::new(expr),
                field,
            };
        }
        Ok(expr)
    }
    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        let token = self
            .bump()
            .ok_or_else(|| ParseError::new(self.context, "expected expression"))?;
        let token_span = token.span;
        match token.kind {
            TokenKind::String(value) => Ok(Expr::String {
                span: token_span,
                value,
            }),
            TokenKind::Integer(value) => Ok(Expr::Int {
                span: token_span,
                value,
            }),
            TokenKind::Float(value) => Ok(Expr::Float {
                span: token_span,
                value: value.to_string(),
            }),
            TokenKind::Duration { magnitude, unit } => Ok(Expr::Duration(DurationLiteral {
                span: token_span,
                magnitude,
                unit,
            })),
            TokenKind::Keyword(Keyword::True) => Ok(Expr::Bool {
                span: token_span,
                value: true,
            }),
            TokenKind::Keyword(Keyword::False) => Ok(Expr::Bool {
                span: token_span,
                value: false,
            }),
            TokenKind::Identifier(name) => {
                if self.peek_kind(&TokenKind::LeftParen) {
                    return Err(ParseError::new(
                        token_span,
                        "call expressions are only allowed as do targets",
                    ));
                }
                Ok(Expr::Ref {
                    span: token_span,
                    name,
                })
            }
            TokenKind::TypeIdentifier(name) if self.eat(&TokenKind::LeftParen) => {
                self.parse_record(token_span, name)
            }
            TokenKind::LeftBracket => self.parse_list(token_span),
            TokenKind::LeftParen => {
                let expr = self.parse_or()?;
                self.expect(&TokenKind::RightParen, "expected `)` after expression")?;
                Ok(expr)
            }
            _ => Err(ParseError::new(token_span, "expected expression")),
        }
    }
    fn parse_list(&mut self, start: Span) -> Result<Expr, ParseError> {
        let mut items = Vec::new();
        if self.eat(&TokenKind::RightBracket) {
            return Ok(Expr::List { span: start, items });
        }
        loop {
            items.push(self.parse_or()?);
            if self.eat(&TokenKind::RightBracket) {
                break;
            }
            self.expect(&TokenKind::Comma, "expected `,` between list items")?;
        }
        let end = items.last().map_or(start, Spanned::span);
        Ok(Expr::List {
            span: join_span(start, end),
            items,
        })
    }
    fn parse_record(&mut self, start: Span, name: String) -> Result<Expr, ParseError> {
        let mut fields = Vec::new();
        if self.eat(&TokenKind::RightParen) {
            return Ok(Expr::Record {
                span: start,
                name,
                fields,
            });
        }
        loop {
            let token = self
                .bump()
                .ok_or_else(|| ParseError::new(start, "unterminated record construction"))?;
            let token_span = token.span;
            let TokenKind::Identifier(field) = token.kind else {
                return Err(ParseError::new(token_span, "record field needs a name"));
            };
            self.expect(&TokenKind::Colon, "record field needs `:`")?;
            let value = self.parse_or()?;
            let field_span = join_span(token_span, value.span());
            fields.push(RecordField {
                span: field_span,
                name: field,
                value,
            });
            if self.eat(&TokenKind::RightParen) {
                break;
            }
            self.expect(&TokenKind::Comma, "unterminated record construction")?;
        }
        let end = fields.last().map_or(start, |field| field.span);
        Ok(Expr::Record {
            span: join_span(start, end),
            name,
            fields,
        })
    }
    fn parse_binary(
        &mut self,
        sub: fn(&mut Self) -> Result<Expr, ParseError>,
        kws: &[Keyword],
        ops: &[BinaryOp],
    ) -> Result<Expr, ParseError> {
        let mut expr = sub(self)?;
        loop {
            let Some(idx) = kws.iter().position(|kw| self.peek_keyword(*kw)) else {
                break;
            };
            self.bump();
            let right = sub(self)?;
            let span = join_span(expr.span(), right.span());
            expr = Expr::Binary {
                span,
                left: Box::new(expr),
                op: ops[idx],
                right: Box::new(right),
            };
        }
        Ok(expr)
    }
    fn comparison_op(&self) -> Option<BinaryOp> {
        self.tokens
            .get(self.pos)
            .and_then(|token| match &token.kind {
                TokenKind::EqualEqual => Some(BinaryOp::Eq),
                TokenKind::BangEqual => Some(BinaryOp::Ne),
                TokenKind::Less => Some(BinaryOp::Lt),
                TokenKind::LessEqual => Some(BinaryOp::Le),
                TokenKind::Greater => Some(BinaryOp::Gt),
                TokenKind::GreaterEqual => Some(BinaryOp::Ge),
                _ => None,
            })
    }
    fn peek_keyword(&self, kw: Keyword) -> bool {
        self.tokens
            .get(self.pos)
            .is_some_and(|t| t.kind == TokenKind::Keyword(kw))
    }
    fn eat_keyword(&mut self, kw: Keyword) -> bool {
        if self.peek_keyword(kw) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn peek_kind(&self, kind: &TokenKind) -> bool {
        self.tokens.get(self.pos).is_some_and(|t| &t.kind == kind)
    }
    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.peek_kind(kind) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, kind: &TokenKind, msg: &str) -> Result<(), ParseError> {
        if self.eat(kind) {
            Ok(())
        } else {
            Err(ParseError::new(
                self.tokens.get(self.pos).map_or(self.context, |t| t.span),
                msg,
            ))
        }
    }
    fn bump(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.pos).cloned();
        self.pos += usize::from(token.is_some());
        token
    }
}

fn unit_text(unit: DurationUnit) -> &'static str {
    match unit {
        DurationUnit::Seconds => "s",
        DurationUnit::Minutes => "m",
        DurationUnit::Hours => "h",
        DurationUnit::Days => "d",
    }
}

pub(crate) fn duration_text(duration: &DurationLiteral) -> String {
    format!("{}{}", duration.magnitude, unit_text(duration.unit))
}
