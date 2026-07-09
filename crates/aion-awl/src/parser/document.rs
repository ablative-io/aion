use crate::ast::{
    AboutDecl, ActionDecl, ActionFieldTag, Comment, Document, Expr, FieldDecl, IoDecl, Spanned,
    Trivia, WorkflowDecl, join_span,
};

use super::ParseError;
use super::calls::comma_parts;
use super::expressions::parse_expr_at;
use super::source::{SourceLine, SourceLines, keyword_rest, span};
use super::step_fields::{
    parse_duration_field, parse_retry, parse_string_field, push_leading, push_trailing,
    reject_duplicate,
};
use super::type_decls::parse_type_decl;
use super::types::parse_type_at;

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

pub(super) struct LineParser {
    pub(super) lines: Vec<SourceLine>,
    pub(super) own_comments: Vec<Comment>,
    pub(super) descriptions: Vec<super::source::DescriptionLine>,
    pub(super) pos: usize,
    pub(super) comment_pos: usize,
    pub(super) description_pos: usize,
}

impl LineParser {
    fn new(source: SourceLines) -> Self {
        Self {
            lines: source.lines,
            own_comments: source.comments,
            descriptions: source.descriptions,
            pos: 0,
            comment_pos: 0,
            description_pos: 0,
        }
    }

    fn parse_document(mut self) -> Result<Document, ParseError> {
        let workflow = self.parse_workflow()?;
        let about = self.parse_maybe_about(0);
        let mut inputs = Vec::new();
        let mut output = None;
        let mut error = None;
        let mut signals = Vec::new();
        let mut types = Vec::new();
        let mut actions = Vec::new();
        let mut steps = Vec::new();
        let mut finish = None;
        let mut finish_leading = Vec::new();
        let mut finish_trailing = None;
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
                "type" => types.push(parse_type_decl(&mut self)?),
                "action" => actions.push(self.parse_action_decl()?),
                "step" => steps.push(self.parse_step()?),
                "finish" => {
                    let parsed_finish = self.parse_finish_decl()?;
                    finish = Some(parsed_finish.0);
                    finish_leading = parsed_finish.1;
                    finish_trailing = parsed_finish.2;
                }
                _ => {
                    return Err(ParseError::new(
                        line.span,
                        format!("unknown declaration `{first}`"),
                    ));
                }
            }
        }
        let finish = finish.ok_or_else(|| {
            let end = self.lines.last().map_or(workflow.span, |line| line.span);
            ParseError::new(end, "missing finish declaration at document end")
        })?;
        let span = join_span(workflow.span, finish.span());
        // Any own-line comments not yet claimed as leading trivia for some
        // line sit after the `finish` declaration, at the end of the
        // document, since `finish` is always the last line.
        let epilogue_comments = self.own_comments[self.comment_pos..].to_vec();
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
            finish_trailing,
            epilogue_comments,
            comments,
        })
    }

    fn parse_finish_decl(&mut self) -> Result<(Expr, Vec<Comment>, Option<Comment>), ParseError> {
        let line = self.bump_required("missing finish declaration after peek")?;
        let trivia = self.take_trivia(&line);
        let rest = keyword_rest(&line, "finish", "finish declaration needs an expression")?;
        let finish = parse_expr_at(&line, rest)?;
        if self.peek().is_some() {
            return Err(ParseError::new(
                line.span,
                "finish must be the final declaration",
            ));
        }
        Ok((finish, trivia.leading, trivia.trailing))
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

    fn parse_maybe_about(&mut self, indent: usize) -> Option<AboutDecl> {
        let line = self.peek()?;
        if line.indent != indent {
            return None;
        }
        // Classification and extraction are a single, inseparable operation:
        // the line is an `about` declaration only if its code is the bare
        // keyword `about` or `about` immediately followed by whitespace, and
        // the declaration text is captured by the very `strip_prefix` that
        // decides it. A line that only *looks* like `about` under
        // Unicode-whitespace word splitting — e.g. a leading no-break space
        // before the keyword — fails byte-level extraction and is left
        // unconsumed, so the document loop reports it through the normal
        // unknown-declaration error path rather than the line being silently
        // bumped and dropped.
        let text = about_text(&line.code)?.to_owned();
        let line = self.bump()?;
        let trivia = self.take_trivia(&line);
        Some(AboutDecl {
            span: line.span,
            trivia,
            text,
        })
    }

    fn parse_io(&mut self, keyword: &str) -> Result<IoDecl, ParseError> {
        let line = self.bump_required("missing IO declaration after peek")?;
        let trivia = self.take_trivia(&line);
        let rest = keyword_rest(&line, keyword, "IO declaration keyword mismatch")?;
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

    fn parse_action_decl(&mut self) -> Result<ActionDecl, ParseError> {
        let line = self.bump_required("missing action declaration after peek")?;
        let trivia = self.take_trivia(&line);
        let sig = keyword_rest(&line, "action", "action declaration needs `-> ReturnType`")?;
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
                description: None,
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
            trailing_comments: Vec::new(),
        };
        if self.peek().is_some_and(|next| next.indent == 2) {
            while self.peek().is_some_and(|next| next.indent == 2) {
                let field = self.bump_required("missing action field after peek")?;
                let field_trivia = self.take_trivia(&field);
                let tag = match first_word(&field.code) {
                    "queue" => {
                        reject_duplicate(action.queue.is_some(), field.span, "queue")?;
                        action.queue = Some(parse_string_field(&field, "queue")?);
                        ActionFieldTag::Queue
                    }
                    "node" => {
                        reject_duplicate(action.node.is_some(), field.span, "node")?;
                        action.node = Some(parse_string_field(&field, "node")?);
                        ActionFieldTag::Node
                    }
                    "timeout" => {
                        reject_duplicate(action.timeout.is_some(), field.span, "timeout")?;
                        action.timeout = Some(parse_duration_field(&field, "timeout")?);
                        ActionFieldTag::Timeout
                    }
                    "retry" => {
                        reject_duplicate(action.retry.is_some(), field.span, "retry")?;
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
                push_leading(&mut action.leading_comments, tag, field_trivia.leading);
                push_trailing(&mut action.trailing_comments, tag, field_trivia.trailing);
            }
        }
        Ok(action)
    }

    pub(super) fn peek(&self) -> Option<&SourceLine> {
        self.lines.get(self.pos)
    }
    pub(super) fn bump(&mut self) -> Option<SourceLine> {
        let line = self.lines.get(self.pos).cloned();
        self.pos += usize::from(line.is_some());
        line
    }
    pub(super) fn bump_required(&mut self, msg: &str) -> Result<SourceLine, ParseError> {
        self.bump()
            .ok_or_else(|| ParseError::new(span(0, 0, 1, 1), msg))
    }

    pub(super) fn take_trivia(&mut self, line: &SourceLine) -> Trivia {
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

    pub(super) fn take_description(&mut self, line: &SourceLine, indent: usize) -> Option<String> {
        let start = self.description_pos;
        while self.description_pos < self.descriptions.len()
            && self.descriptions[self.description_pos].span.start < line.span.start
        {
            self.description_pos += 1;
        }
        let preceding = &self.descriptions[start..self.description_pos];
        let mut expected_line = line.span.line;
        let mut lines = Vec::new();
        for description in preceding.iter().rev() {
            if description.indent != indent || description.span.line + 1 != expected_line {
                break;
            }
            lines.push(description.text.clone());
            expected_line = description.span.line;
        }
        if lines.is_empty() {
            None
        } else {
            lines.reverse();
            Some(lines.join("\n"))
        }
    }
}

pub(super) fn first_word(code: &str) -> &str {
    code.split_whitespace().next().unwrap_or("")
}

/// Recognise an `about` declaration and return its trimmed text in one step.
///
/// Returns `Some(text)` only when `code` begins with the whole keyword
/// `about` — the bare keyword or `about` followed by whitespace — mirroring
/// exactly which lines `first_word(code) == "about"` classified while also
/// performing the byte-level extraction. Returning `None` (rather than
/// bumping the line and discarding it) keeps classification and extraction
/// inseparable, so a line that cannot be extracted is never silently
/// consumed.
fn about_text(code: &str) -> Option<&str> {
    let rest = code.strip_prefix("about")?;
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest.trim_start())
    } else {
        None
    }
}
