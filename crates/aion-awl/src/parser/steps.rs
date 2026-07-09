use crate::Span;
use crate::ast::{HandlerBlock, HandlerTerminal, StepDecl, join_span};

use super::ParseError;
use super::calls::parse_do;
use super::document::{LineParser, first_word};
use super::expressions::parse_expr_at;
use super::source::keyword_rest;
use super::step_fields::StepFields;

impl LineParser {
    pub(super) fn parse_step(&mut self) -> Result<StepDecl, ParseError> {
        let head = self.bump_required("missing step declaration after peek")?;
        let trivia = self.take_trivia(&head);
        let name = keyword_rest(&head, "step ", "step declaration needs a name")?
            .trim()
            .to_owned();
        let mut fields = StepFields::default();
        while self.peek().is_some_and(|line| line.indent == 2) {
            let line = self.bump_required("missing step field after peek")?;
            fields.parse_line(self, &line)?;
        }
        if let Some(line) = self
            .peek()
            .filter(|line| line.indent > 0 && line.indent != 2)
        {
            return Err(ParseError::new(
                line.span,
                "wrong indentation depth for step field or handler block body",
            ));
        }
        let op = fields.op.ok_or_else(|| {
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
            about: fields.about,
            when: fields.when,
            each: fields.each,
            op,
            repeat: fields.repeat,
            until: fields.until,
            retry: fields.retry,
            timeout: fields.timeout,
            on_timeout: fields.on_timeout,
            on_failure: fields.on_failure,
            bind_as: fields.bind_as,
            queue: fields.queue,
            node: fields.node,
            leading_comments: fields.leading_comments,
            trailing_comments: fields.trailing_comments,
        })
    }

    pub(super) fn parse_handler(&mut self, head: Span) -> Result<HandlerBlock, ParseError> {
        if self.peek().is_none_or(|line| line.indent != 4) {
            let err = self.peek().map_or(head, |line| line.span);
            return Err(ParseError::new(
                err,
                "wrong indentation depth for a handler block body",
            ));
        }
        let mut actions = Vec::new();
        let mut action_leading = Vec::new();
        let mut action_trailing = Vec::new();
        let mut terminal: Option<HandlerTerminal> = None;
        let mut terminal_leading = Vec::new();
        let mut terminal_trailing = None;
        while self.peek().is_some_and(|line| line.indent == 4) {
            let line = self.bump_required("missing handler field after peek")?;
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
                    action_trailing.push(trivia.trailing);
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
                    terminal_trailing = trivia.trailing;
                    terminal = Some(HandlerTerminal::Finish(parse_expr_at(
                        &line,
                        keyword_rest(&line, "finish", "finish handler needs an expression")?,
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
                    terminal_trailing = trivia.trailing;
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
            action_trailing,
            terminal,
            terminal_leading,
            terminal_trailing,
        })
    }
}
