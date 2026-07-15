//! Worker blocks, action requirements, per-action config lines, and child
//! workflow declarations.

use crate::ast::{
    ActionDecl, ChildDecl, ConfigLine, ConfigValue, DocLine, DurationLiteral, Lead, ParamDecl,
    RetrySpec, TypeRef, join_span,
};
use crate::{Keyword, Span, TokenKind};

use super::ParseError;
use super::stream::{Stream, describe};
use super::types::{parse_type_ref, type_ref_span};

/// Parse a `worker` block; the `worker` keyword has been consumed.
pub(super) fn parse_worker(
    stream: &mut Stream,
    lead: Vec<Lead>,
    docs: Vec<DocLine>,
    keyword_span: Span,
) -> Result<crate::ast::WorkerDecl, ParseError> {
    let (name, name_span) = stream.expect_name("a worker name")?;
    let span = join_span(keyword_span, name_span);
    let trailing = stream.end_line()?;

    let mut actions = Vec::new();
    let outer_leads = stream.take_leads()?;
    if stream.open_block() {
        stream.push_back_leads(outer_leads);
        loop {
            let action_lead = stream.take_leads()?;
            if stream.at_item_block_end() {
                stream.push_back_leads(action_lead);
                break;
            }
            let action_docs = stream.take_docs();
            actions.push(parse_action(stream, action_lead, action_docs)?);
        }
        let stray = stream.take_leads()?;
        stream.consume_block_dedent();
        stream.push_back_leads(stray);
    } else {
        stream.push_back_leads(outer_leads);
    }

    if actions.is_empty() {
        return Err(ParseError::new(
            span,
            format!(
                "worker `{name}` declares no actions; a worker block requires at least one `action`"
            ),
        ));
    }

    Ok(crate::ast::WorkerDecl {
        span,
        lead,
        docs,
        trailing,
        name,
        name_span,
        actions,
    })
}

fn parse_action(
    stream: &mut Stream,
    lead: Vec<Lead>,
    docs: Vec<DocLine>,
) -> Result<ActionDecl, ParseError> {
    let Ok(keyword) = stream.expect(
        &TokenKind::Keyword(Keyword::Action),
        "expected an `action` declaration inside the worker block",
    ) else {
        // A `child` nested inside a worker gets a targeted diagnostic.
        let Some(token) = stream.peek() else {
            return Err(ParseError::new(
                stream.eof_span(),
                "expected an `action` declaration inside the worker block, found end of input",
            ));
        };
        if matches!(token.kind, TokenKind::Keyword(Keyword::Child)) {
            return Err(ParseError::new(
                token.span,
                "`child` workflows are declared outside worker blocks: the engine \
                 routes children, not a queue",
            ));
        }
        return Err(ParseError::new(
            token.span,
            format!(
                "expected an `action` declaration inside the worker block, found {}",
                describe(&token.kind)
            ),
        ));
    };
    let (name, name_span) = stream.expect_name("an action name")?;
    let params = parse_params(stream, "action")?;
    let returns = parse_returns(stream, keyword.span, name_span, "action")?;
    let trailing = stream.end_line()?;
    let config = parse_config_block(stream)?;
    Ok(ActionDecl {
        span: join_span(keyword.span, name_span),
        lead,
        docs,
        trailing,
        name,
        name_span,
        params,
        returns,
        config,
    })
}

/// Parse a `child` declaration; the `child` keyword has been consumed.
pub(super) fn parse_child(
    stream: &mut Stream,
    lead: Vec<Lead>,
    docs: Vec<DocLine>,
    keyword_span: Span,
) -> Result<ChildDecl, ParseError> {
    let (name, name_span) = stream.expect_name("a child workflow name")?;
    let params = parse_params(stream, "child")?;
    let returns = parse_returns(stream, keyword_span, name_span, "child")?;
    let trailing = stream.end_line()?;
    Ok(ChildDecl {
        span: join_span(keyword_span, name_span),
        lead,
        docs,
        trailing,
        name,
        name_span,
        params,
        returns,
    })
}

pub(super) fn parse_params(stream: &mut Stream, what: &str) -> Result<Vec<ParamDecl>, ParseError> {
    stream.expect(
        &TokenKind::LeftParen,
        "expected `(` to open the parameter list",
    )?;
    let mut params = Vec::new();
    loop {
        if stream
            .eat(|kind| matches!(kind, TokenKind::RightParen))
            .is_some()
        {
            return Ok(params);
        }
        let (name, name_span) = stream.expect_name("a parameter name")?;
        if stream
            .eat(|kind| matches!(kind, TokenKind::Colon))
            .is_none()
        {
            return Err(ParseError::new(
                name_span,
                format!("every {what} parameter needs a type: write `{name}: <Type>`"),
            ));
        }
        let ty = parse_type_ref(stream)?;
        params.push(ParamDecl {
            span: join_span(name_span, type_ref_span(&ty)),
            name,
            name_span,
            ty,
        });
        if stream
            .eat(|kind| matches!(kind, TokenKind::Comma))
            .is_some()
        {
            continue;
        }
        stream.expect(
            &TokenKind::RightParen,
            "expected `)` to close the parameter list",
        )?;
        return Ok(params);
    }
}

fn parse_returns(
    stream: &mut Stream,
    keyword_span: Span,
    name_span: Span,
    what: &str,
) -> Result<TypeRef, ParseError> {
    if stream
        .eat(|kind| matches!(kind, TokenKind::Arrow))
        .is_none()
    {
        return Err(ParseError::new(
            join_span(keyword_span, name_span),
            format!(
                "every {what} declares its result with `-> <Type>` (use `Nil` for effect-only {what}s)"
            ),
        ));
    }
    parse_type_ref(stream)
}

/// Parse an optional indented config block (one config line) following an
/// action declaration or a call statement.
pub(super) fn parse_config_block(stream: &mut Stream) -> Result<Option<ConfigLine>, ParseError> {
    let leads = stream.take_leads()?;
    let opens_config = stream.peek_is(|kind| matches!(kind, TokenKind::Indent))
        && stream.peek_second().is_some_and(|token| {
            matches!(
                token.kind,
                TokenKind::Keyword(Keyword::Node | Keyword::Timeout | Keyword::Retry)
            )
        });
    if !opens_config {
        stream.push_back_leads(leads);
        return Ok(None);
    }
    stream.eat(|kind| matches!(kind, TokenKind::Indent));
    let config = parse_config_line(stream, leads)?;
    let stray = stream.take_leads()?;
    stream.consume_block_dedent();
    stream.push_back_leads(stray);
    Ok(Some(config))
}

fn parse_config_line(stream: &mut Stream, lead: Vec<Lead>) -> Result<ConfigLine, ParseError> {
    let start = stream.peek_span();
    let mut node = None;
    let mut timeout = None;
    let mut retry = None;
    loop {
        match stream.peek() {
            Some(token) => {
                let key_span = token.span;
                match token.kind {
                    TokenKind::Keyword(Keyword::Node) => {
                        stream.next();
                        if node.is_some() {
                            return Err(ParseError::new(key_span, "duplicate `node` key"));
                        }
                        let (name, span) = stream.expect_name("a node name after `node`")?;
                        node = Some(ConfigValue { span, name });
                    }
                    TokenKind::Keyword(Keyword::Timeout) => {
                        stream.next();
                        if timeout.is_some() {
                            return Err(ParseError::new(key_span, "duplicate `timeout` key"));
                        }
                        timeout = Some(expect_duration(
                            stream,
                            key_span,
                            "`timeout` needs a duration value (`30s`, `5m`, `3h`, `2d`)",
                        )?);
                    }
                    TokenKind::Keyword(Keyword::Retry) => {
                        stream.next();
                        if retry.is_some() {
                            return Err(ParseError::new(key_span, "duplicate `retry` key"));
                        }
                        retry = Some(parse_retry(stream, key_span)?);
                    }
                    ref other => {
                        return Err(ParseError::new(
                            key_span,
                            format!(
                                "expected a config key (`node`, `timeout`, `retry`), found {}",
                                describe(other)
                            ),
                        ));
                    }
                }
            }
            None => {
                return Err(ParseError::new(
                    stream.eof_span(),
                    "expected a config key (`node`, `timeout`, `retry`), found end of input",
                ));
            }
        }
        if stream
            .eat(|kind| matches!(kind, TokenKind::Comma))
            .is_some()
        {
            continue;
        }
        let trailing = stream.end_line()?;
        return Ok(ConfigLine {
            span: start,
            lead,
            trailing,
            node,
            timeout,
            retry,
        });
    }
}

fn parse_retry(stream: &mut Stream, retry_span: Span) -> Result<RetrySpec, ParseError> {
    let count = match stream.peek() {
        Some(token) => match token.kind {
            TokenKind::Integer(value) => {
                stream.next();
                value
            }
            ref other => {
                return Err(ParseError::new(
                    token.span,
                    format!(
                        "expected a retry count after `retry`, found {}",
                        describe(other)
                    ),
                ));
            }
        },
        None => {
            return Err(ParseError::new(
                retry_span,
                "expected a retry count after `retry`, found end of input",
            ));
        }
    };
    match stream.peek() {
        Some(token) if matches!(token.kind, TokenKind::Keyword(Keyword::Every)) => {
            stream.next();
            let every = expect_duration(
                stream,
                retry_span,
                "`retry N every` needs a duration (`30s`, `5m`)",
            )?;
            Ok(RetrySpec::Every {
                span: join_span(retry_span, every.span),
                count,
                every,
            })
        }
        Some(token) if matches!(token.kind, TokenKind::Keyword(Keyword::Backoff)) => {
            let backoff_span = token.span;
            stream.next();
            let min = expect_duration(
                stream,
                backoff_span,
                "`retry N backoff` needs a duration range (`10s..5m`)",
            )?;
            if stream
                .eat(|kind| matches!(kind, TokenKind::DotDot))
                .is_none()
            {
                return Err(ParseError::new(
                    min.span,
                    "`backoff` takes a duration range: write `<min>..<max>`",
                ));
            }
            let max = expect_duration(
                stream,
                backoff_span,
                "`backoff <min>..` needs a maximum duration",
            )?;
            Ok(RetrySpec::Backoff {
                span: join_span(retry_span, max.span),
                count,
                min,
                max,
            })
        }
        _ => Err(ParseError::new(
            retry_span,
            "`retry` needs a schedule: `every <duration>` or `backoff <min>..<max>`",
        )),
    }
}

/// Expect a duration literal, anchoring the failure at `anchor` when the
/// line ends instead.
pub(super) fn expect_duration(
    stream: &mut Stream,
    anchor: Span,
    message: &str,
) -> Result<DurationLiteral, ParseError> {
    match stream.peek() {
        Some(token) => match token.kind {
            TokenKind::Duration { magnitude, unit } => {
                let literal = DurationLiteral {
                    span: token.span,
                    magnitude,
                    unit,
                };
                stream.next();
                Ok(literal)
            }
            TokenKind::Newline | TokenKind::Comment(_) => {
                Err(ParseError::new(anchor, message.to_owned()))
            }
            _ => Err(ParseError::new(token.span, message.to_owned())),
        },
        None => Err(ParseError::new(anchor, message.to_owned())),
    }
}
