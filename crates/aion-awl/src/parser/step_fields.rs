use crate::ast::{
    AboutDecl, BindDecl, Comment, DurationLiteral, EachSpec, Expr, HandlerBlock, RetrySpec,
    StepFieldTag, StepOp, Trivia,
};
use crate::{Keyword, Span, Token, TokenKind};

use super::ParseError;
use super::calls::parse_do;
use super::document::{LineParser, first_word};
use super::expressions::parse_expr_at;
use super::source::{SourceLine, keyword_rest, lex_at};

#[derive(Default)]
pub(super) struct StepFields {
    pub(super) about: Option<AboutDecl>,
    pub(super) when: Option<Expr>,
    pub(super) each: Option<EachSpec>,
    pub(super) op: Option<StepOp>,
    pub(super) repeat: Option<Expr>,
    pub(super) until: Option<Expr>,
    pub(super) retry: Option<RetrySpec>,
    pub(super) timeout: Option<DurationLiteral>,
    pub(super) on_timeout: Option<HandlerBlock>,
    pub(super) on_failure: Option<HandlerBlock>,
    pub(super) bind_as: Option<BindDecl>,
    pub(super) queue: Option<String>,
    pub(super) node: Option<String>,
    pub(super) leading_comments: Vec<(StepFieldTag, Vec<Comment>)>,
    pub(super) trailing_comments: Vec<(StepFieldTag, Comment)>,
}

impl StepFields {
    pub(super) fn parse_line(
        &mut self,
        parser: &mut LineParser,
        line: &SourceLine,
    ) -> Result<(), ParseError> {
        match first_word(&line.code) {
            "about" => self.parse_about(parser, line),
            "when" => self.parse_when(parser, line),
            "each" => self.parse_each_field(parser, line),
            "do" => self.parse_do_field(parser, line),
            "wait" => self.parse_wait(parser, line),
            "sleep" => self.parse_sleep(parser, line),
            "repeat" => self.parse_repeat_field(parser, line),
            "until" => self.parse_until(parser, line),
            "retry" => self.parse_retry_field(parser, line),
            "timeout" => self.parse_timeout(parser, line),
            "on" if line.code == "on timeout" => self.parse_on_timeout(parser, line),
            "on" if line.code == "on failure" => self.parse_on_failure(parser, line),
            "as" => self.parse_bind(parser, line),
            "queue" => self.parse_queue(parser, line),
            "node" => self.parse_node(parser, line),
            other => Err(ParseError::new(
                line.span,
                format!("unknown step field `{other}`"),
            )),
        }
    }

    fn parse_about(
        &mut self,
        parser: &mut LineParser,
        line: &SourceLine,
    ) -> Result<(), ParseError> {
        self.about = Some(AboutDecl {
            span: line.span,
            trivia: parser.take_trivia(line),
            text: keyword_rest(line, "about", "about field needs text")?.to_owned(),
        });
        Ok(())
    }

    fn parse_when(&mut self, parser: &mut LineParser, line: &SourceLine) -> Result<(), ParseError> {
        reject_duplicate(self.when.is_some(), line.span, "when")?;
        self.add_trivia(parser, line, StepFieldTag::When);
        self.when = Some(parse_expr_at(
            line,
            keyword_rest(line, "when", "when field needs an expression")?,
        )?);
        Ok(())
    }

    fn parse_each_field(
        &mut self,
        parser: &mut LineParser,
        line: &SourceLine,
    ) -> Result<(), ParseError> {
        reject_duplicate(self.each.is_some(), line.span, "each")?;
        self.add_trivia(parser, line, StepFieldTag::Each);
        self.each = Some(parse_each(line)?);
        Ok(())
    }

    fn parse_do_field(
        &mut self,
        parser: &mut LineParser,
        line: &SourceLine,
    ) -> Result<(), ParseError> {
        self.add_trivia(parser, line, StepFieldTag::Op);
        set_op(&mut self.op, StepOp::Do(parse_do(line)?), line.span)
    }

    fn parse_wait(&mut self, parser: &mut LineParser, line: &SourceLine) -> Result<(), ParseError> {
        self.add_trivia(parser, line, StepFieldTag::Op);
        set_op(
            &mut self.op,
            StepOp::Wait {
                span: line.span,
                signal: keyword_rest(line, "wait", "wait field needs a signal")?
                    .trim()
                    .to_owned(),
            },
            line.span,
        )
    }

    fn parse_sleep(
        &mut self,
        parser: &mut LineParser,
        line: &SourceLine,
    ) -> Result<(), ParseError> {
        self.add_trivia(parser, line, StepFieldTag::Op);
        set_op(
            &mut self.op,
            StepOp::Sleep(parse_duration_field(line, "sleep")?),
            line.span,
        )
    }

    fn parse_repeat_field(
        &mut self,
        parser: &mut LineParser,
        line: &SourceLine,
    ) -> Result<(), ParseError> {
        reject_duplicate(self.repeat.is_some(), line.span, "repeat")?;
        self.add_trivia(parser, line, StepFieldTag::Repeat);
        self.repeat = Some(parse_repeat(line)?);
        Ok(())
    }

    fn parse_until(
        &mut self,
        parser: &mut LineParser,
        line: &SourceLine,
    ) -> Result<(), ParseError> {
        reject_duplicate(self.until.is_some(), line.span, "until")?;
        self.add_trivia(parser, line, StepFieldTag::Until);
        self.until = Some(parse_expr_at(
            line,
            keyword_rest(line, "until", "until field needs an expression")?,
        )?);
        Ok(())
    }

    fn parse_retry_field(
        &mut self,
        parser: &mut LineParser,
        line: &SourceLine,
    ) -> Result<(), ParseError> {
        reject_duplicate(self.retry.is_some(), line.span, "retry")?;
        self.add_trivia(parser, line, StepFieldTag::Retry);
        self.retry = Some(parse_retry(line)?);
        Ok(())
    }

    fn parse_timeout(
        &mut self,
        parser: &mut LineParser,
        line: &SourceLine,
    ) -> Result<(), ParseError> {
        reject_duplicate(self.timeout.is_some(), line.span, "timeout")?;
        self.add_trivia(parser, line, StepFieldTag::Timeout);
        self.timeout = Some(parse_duration_field(line, "timeout")?);
        Ok(())
    }

    fn parse_on_timeout(
        &mut self,
        parser: &mut LineParser,
        line: &SourceLine,
    ) -> Result<(), ParseError> {
        reject_duplicate(self.on_timeout.is_some(), line.span, "on timeout")?;
        self.add_trivia(parser, line, StepFieldTag::OnTimeout);
        self.on_timeout = Some(parser.parse_handler(line.span)?);
        Ok(())
    }

    fn parse_on_failure(
        &mut self,
        parser: &mut LineParser,
        line: &SourceLine,
    ) -> Result<(), ParseError> {
        reject_duplicate(self.on_failure.is_some(), line.span, "on failure")?;
        self.add_trivia(parser, line, StepFieldTag::OnFailure);
        self.on_failure = Some(parser.parse_handler(line.span)?);
        Ok(())
    }

    fn parse_bind(&mut self, parser: &mut LineParser, line: &SourceLine) -> Result<(), ParseError> {
        reject_duplicate(self.bind_as.is_some(), line.span, "as")?;
        self.bind_as = Some(BindDecl {
            span: line.span,
            trivia: parser.take_trivia(line),
            name: keyword_rest(line, "as", "as field needs a binding name")?
                .trim()
                .to_owned(),
        });
        Ok(())
    }

    fn parse_queue(
        &mut self,
        parser: &mut LineParser,
        line: &SourceLine,
    ) -> Result<(), ParseError> {
        reject_duplicate(self.queue.is_some(), line.span, "queue")?;
        self.add_trivia(parser, line, StepFieldTag::Queue);
        self.queue = Some(parse_string_field(line, "queue")?);
        Ok(())
    }

    fn parse_node(&mut self, parser: &mut LineParser, line: &SourceLine) -> Result<(), ParseError> {
        reject_duplicate(self.node.is_some(), line.span, "node")?;
        self.add_trivia(parser, line, StepFieldTag::Node);
        self.node = Some(parse_string_field(line, "node")?);
        Ok(())
    }

    fn add_trivia(&mut self, parser: &mut LineParser, line: &SourceLine, tag: StepFieldTag) {
        let trivia = parser.take_trivia(line);
        self.push_trivia(tag, trivia);
    }

    fn push_trivia(&mut self, tag: StepFieldTag, trivia: Trivia) {
        push_leading(&mut self.leading_comments, tag, trivia.leading);
        push_trailing(&mut self.trailing_comments, tag, trivia.trailing);
    }
}

pub(super) fn parse_each(line: &SourceLine) -> Result<EachSpec, ParseError> {
    let rest = keyword_rest(line, "each", "each field needs `name in expr`")?;
    let (name, expr) = rest
        .split_once(" in ")
        .ok_or_else(|| ParseError::new(line.span, "each field needs `name in expr`"))?;
    Ok(EachSpec {
        span: line.span,
        name: name.trim().to_owned(),
        in_expr: parse_expr_at(line, expr.trim())?,
    })
}

pub(super) fn parse_repeat(line: &SourceLine) -> Result<Expr, ParseError> {
    let rest = keyword_rest(line, "repeat", "repeat field needs `up to expr`")?;
    let expr = rest
        .strip_prefix("up to")
        .ok_or_else(|| ParseError::new(line.span, "repeat field needs `up to expr`"))?
        .trim_start();
    parse_expr_at(line, expr)
}

pub(super) fn parse_string_field(line: &SourceLine, keyword: &str) -> Result<String, ParseError> {
    let rest = keyword_rest(
        line,
        keyword,
        &format!("{keyword} field needs a string literal"),
    )?
    .trim();
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

pub(super) fn parse_duration_field(
    line: &SourceLine,
    keyword: &str,
) -> Result<DurationLiteral, ParseError> {
    let rest = keyword_rest(line, keyword, "expected duration literal")?.trim();
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

pub(super) fn parse_retry(line: &SourceLine) -> Result<RetrySpec, ParseError> {
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
pub(super) fn push_leading<T>(
    leading_comments: &mut Vec<(T, Vec<Comment>)>,
    tag: T,
    leading: Vec<Comment>,
) {
    if !leading.is_empty() {
        leading_comments.push((tag, leading));
    }
}

/// Record a same-line trailing comment for `tag`, if the field's line had one.
pub(super) fn push_trailing<T>(
    trailing_comments: &mut Vec<(T, Comment)>,
    tag: T,
    trailing: Option<Comment>,
) {
    if let Some(comment) = trailing {
        trailing_comments.push((tag, comment));
    }
}

/// Reject a field that has already been set once (every AWL-0 step/action
/// field is single-valued); the error points at the *second* occurrence.
pub(super) fn reject_duplicate(
    already_set: bool,
    span: Span,
    field: &str,
) -> Result<(), ParseError> {
    if already_set {
        return Err(ParseError::new(
            span,
            format!("duplicate `{field}` field; only one is allowed"),
        ));
    }
    Ok(())
}
