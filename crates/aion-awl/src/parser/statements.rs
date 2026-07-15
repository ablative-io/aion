//! Body statements: calls with bindings and call-site config overrides,
//! spawns, pipe chains with the fixed combinator vocabulary, waits,
//! sleeps, route lines, and the dispatch that tells them apart (including
//! the dead-keyword migration fix-its).

use crate::ast::{
    Call, CallStmt, CombinatorCall, CombinatorKind, DocLine, Lead, PipeEnd, PipeStage, PipeStmt,
    RouteStmt, RouteTarget, SleepStmt, SpawnStmt, Statement, WaitStmt, join_span,
};
use crate::{Keyword, Span, TokenKind};

use super::ParseError;
use super::args::{parse_args, parse_binding};
use super::exprs::parse_expr;
use super::hints::gone_keyword_hint;
use super::steps::{parse_fork, parse_loop, parse_step};
use super::stream::{Stream, describe};
use super::workers::{expect_duration, parse_config_block};

pub(super) fn reject_docs(docs: &[DocLine], what: &str) -> Result<(), ParseError> {
    match docs.first() {
        Some(doc) => Err(ParseError::new(
            doc.span,
            format!(
                "`///` doc lines attach to declarations (types, fields, actions, steps), not {what}"
            ),
        )),
        None => Ok(()),
    }
}

/// Parse one body statement (step, fork, loop, and on-failure bodies share
/// this dispatch).
pub(super) fn parse_statement(
    stream: &mut Stream,
    lead: Vec<Lead>,
    docs: Vec<DocLine>,
) -> Result<Statement, ParseError> {
    let Some(token) = stream.peek() else {
        return Err(ParseError::new(
            stream.eof_span(),
            "expected a statement, found end of input",
        ));
    };
    let span = token.span;
    match &token.kind {
        TokenKind::Keyword(Keyword::Step) => {
            stream.next();
            let step = parse_step(stream, lead, docs, span)?;
            Ok(Statement::SubStep(Box::new(step)))
        }
        TokenKind::Keyword(Keyword::Loop) => {
            reject_docs(&docs, "a loop")?;
            stream.next();
            Ok(Statement::Loop(parse_loop(stream, lead, span)?))
        }
        TokenKind::Keyword(Keyword::Fork) => {
            reject_docs(&docs, "a fork")?;
            stream.next();
            Ok(Statement::Fork(parse_fork(stream, lead, span)?))
        }
        TokenKind::Keyword(
            keyword @ (Keyword::Distribute | Keyword::Sequence | Keyword::Collect),
        ) => {
            let keyword = *keyword;
            super::flow::parse_region_statement(stream, lead, &docs, keyword, span)
        }
        TokenKind::Keyword(Keyword::Wait) => {
            reject_docs(&docs, "a wait")?;
            stream.next();
            Ok(Statement::Wait(parse_wait(stream, lead, span)?))
        }
        TokenKind::Keyword(Keyword::Sleep) => {
            reject_docs(&docs, "a sleep")?;
            stream.next();
            let duration = expect_duration(
                stream,
                span,
                "`sleep` takes a duration literal (`30s`, `5m`, `3h`, `2d`)",
            )?;
            let trailing = stream.end_line()?;
            Ok(Statement::Sleep(SleepStmt {
                span: join_span(span, duration.span),
                lead,
                trailing,
                duration,
            }))
        }
        TokenKind::Keyword(Keyword::Spawn) => {
            reject_docs(&docs, "a spawn")?;
            stream.next();
            let call = parse_call(stream)?;
            let bind = match stream.eat(|kind| matches!(kind, TokenKind::Arrow)) {
                Some(arrow) => Some(parse_binding(stream, arrow.span)?),
                None => None,
            };
            let trailing = stream.end_line()?;
            Ok(Statement::Spawn(SpawnStmt {
                span: join_span(span, call.span),
                lead,
                trailing,
                call,
                bind,
            }))
        }
        TokenKind::Keyword(Keyword::Route) => {
            reject_docs(&docs, "a route")?;
            stream.next();
            let target = parse_route_target(stream)?;
            let trailing = stream.end_line()?;
            Ok(Statement::Route(RouteStmt {
                span: join_span(span, target.span),
                lead,
                trailing,
                target,
            }))
        }
        TokenKind::FieldAccessor(name) => Err(ParseError::new(
            span,
            format!(
                "`.{name}` is not a statement: a bare field accessor is only a pipe stage \
                 or a combinator argument"
            ),
        )),
        TokenKind::Identifier(name) => {
            reject_docs(&docs, "a statement")?;
            let name = name.clone();
            parse_value_statement(stream, lead, span, &name)
        }
        // A statement may start with any expression: a literal (including
        // raw strings, `json { … }`, and `schema of`), a list, a record
        // construction or variant, or the `workflow` namespace — the chain
        // still has to end in `-> <name>` or `route <target>`.
        other if starts_expression(other) => {
            reject_docs(&docs, "a statement")?;
            parse_pipe_statement(stream, lead, span)
        }
        other => Err(ParseError::new(
            span,
            format!("expected a statement, found {}", describe(other)),
        )),
    }
}

/// Whether a token kind can begin an expression-headed statement (a pipe
/// chain whose head is not a plain identifier).
const fn starts_expression(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::String(_)
            | TokenKind::RawString(_)
            | TokenKind::Integer(_)
            | TokenKind::Float(_)
            | TokenKind::Duration { .. }
            | TokenKind::LeftBracket
            | TokenKind::TypeIdentifier(_)
            | TokenKind::Keyword(
                Keyword::True
                    | Keyword::False
                    | Keyword::Json
                    | Keyword::Schema
                    | Keyword::Workflow
                    | Keyword::Not,
            )
    )
}

/// Parse a statement starting with an identifier: a call (`name(…)`), a
/// pipe chain (`name |> …`), a dead-keyword migration error, or a `=`
/// statement-binder error.
fn parse_value_statement(
    stream: &mut Stream,
    lead: Vec<Lead>,
    start: Span,
    name: &str,
) -> Result<Statement, ParseError> {
    match stream.peek_second().map(|token| &token.kind) {
        Some(TokenKind::LeftParen) => {
            let call = parse_call(stream)?;
            if stream.peek_is(|kind| matches!(kind, TokenKind::Pipe)) {
                return Err(ParseError::new(
                    stream.peek_span(),
                    format!(
                        "a call is not a pipe head: bind its result first — \
                         `{name}(…) -> x` — then pipe the binding (`x |> …`)"
                    ),
                ));
            }
            let bind = match stream.eat(|kind| matches!(kind, TokenKind::Arrow)) {
                Some(arrow) => Some(parse_binding(stream, arrow.span)?),
                None => None,
            };
            let trailing = stream.end_line()?;
            let config = parse_config_block(stream)?;
            Ok(Statement::Call(CallStmt {
                span: call.span,
                lead,
                trailing,
                call,
                bind,
                config,
            }))
        }
        Some(TokenKind::Equal) => {
            stream.next();
            let equal_span = stream.peek_span();
            Err(ParseError::new(
                equal_span,
                format!(
                    "`=` is not a statement binder: bind results with `->` — \
                     `action(args…) -> {name}`"
                ),
            ))
        }
        _ => {
            if let Some(hint) = gone_keyword_hint(name) {
                // Only a dead-keyword *statement* reads as migration debt: a
                // pipe head named like one is still a pipe head.
                if !matches!(
                    stream.peek_second().map(|token| &token.kind),
                    Some(TokenKind::Pipe | TokenKind::Arrow | TokenKind::FieldAccessor(_))
                ) {
                    return Err(ParseError::new(start, hint));
                }
            }
            parse_pipe_statement(stream, lead, start)
        }
    }
}

fn parse_pipe_statement(
    stream: &mut Stream,
    lead: Vec<Lead>,
    start: Span,
) -> Result<Statement, ParseError> {
    let head = parse_expr(stream)?;
    let mut stages = Vec::new();
    let mut end = None;
    loop {
        if stream.peek_is(|kind| matches!(kind, TokenKind::Newline))
            && !stream.continue_wrapped_pipe()
        {
            break;
        }
        if let Some(pipe) = stream.eat(|kind| matches!(kind, TokenKind::Pipe)) {
            if stream.peek_is(|kind| matches!(kind, TokenKind::Keyword(Keyword::Route))) {
                stream.next();
                let target = parse_route_target(stream)?;
                end = Some(PipeEnd::Route(target));
                break;
            }
            stages.push(parse_pipe_stage(stream, pipe.span)?);
            continue;
        }
        if let Some(arrow) = stream.eat(|kind| matches!(kind, TokenKind::Arrow)) {
            end = Some(PipeEnd::Bind(parse_binding(stream, arrow.span)?));
            break;
        }
        break;
    }
    let Some(end) = end else {
        return Err(ParseError::new(
            start,
            "unterminated pipe chain: end with `-> <name>` or `route <target>`",
        ));
    };
    let trailing = stream.end_line()?;
    Ok(Statement::Pipe(PipeStmt {
        span: start,
        lead,
        trailing,
        head,
        stages,
        end,
    }))
}

fn parse_pipe_stage(stream: &mut Stream, pipe_span: Span) -> Result<PipeStage, ParseError> {
    let Some(token) = stream.peek() else {
        return Err(ParseError::new(
            pipe_span,
            "expected a pipe stage after `|>`, found end of input",
        ));
    };
    let span = token.span;
    match &token.kind {
        TokenKind::Identifier(name) => {
            let name = name.clone();
            stream.next();
            Ok(PipeStage::Action { span, name })
        }
        TokenKind::FieldAccessor(name) => {
            let name = name.clone();
            stream.next();
            Ok(PipeStage::Field { span, name })
        }
        TokenKind::Keyword(
            keyword @ (Keyword::Filter
            | Keyword::Map
            | Keyword::Sort
            | Keyword::Count
            | Keyword::Any
            | Keyword::All),
        ) => {
            let kind = match keyword {
                Keyword::Filter => CombinatorKind::Filter,
                Keyword::Map => CombinatorKind::Map,
                Keyword::Sort => CombinatorKind::Sort,
                Keyword::Any => CombinatorKind::Any,
                Keyword::All => CombinatorKind::All,
                Keyword::Count => CombinatorKind::Count,
                _ => return Err(ParseError::new(span, "unknown collection combinator")),
            };
            stream.next();
            let mut full = span;
            let arg = if stream.peek_is(|kind| matches!(kind, TokenKind::LeftParen)) {
                stream.next();
                let value = parse_expr(stream)?;
                let close = stream.expect(
                    &TokenKind::RightParen,
                    "expected `)` to close the combinator argument",
                )?;
                full = join_span(span, close.span);
                Some(value)
            } else {
                None
            };
            Ok(PipeStage::Combinator(CombinatorCall {
                span: full,
                kind,
                arg,
            }))
        }
        other => Err(ParseError::new(
            span,
            format!(
                "expected a pipe stage after `|>`, found {}",
                describe(other)
            ),
        )),
    }
}

fn parse_call(stream: &mut Stream) -> Result<Call, ParseError> {
    let (name, name_span) = stream.expect_name("a call target")?;
    stream.expect(&TokenKind::LeftParen, "expected `(` to open the arguments")?;
    let (args, close) = parse_args(stream)?;
    Ok(Call {
        span: join_span(name_span, close),
        name,
        name_span,
        args,
    })
}

fn parse_wait(
    stream: &mut Stream,
    lead: Vec<Lead>,
    wait_span: Span,
) -> Result<WaitStmt, ParseError> {
    let (signal, signal_span) = stream.expect_name("a signal name after `wait`")?;
    let timeout = if stream
        .eat(|kind| matches!(kind, TokenKind::Keyword(Keyword::Timeout)))
        .is_some()
    {
        Some(expect_duration(
            stream,
            wait_span,
            "`wait … timeout` needs a duration (`30s`, `5m`, `2d`)",
        )?)
    } else {
        None
    };
    let Some(arrow) = stream.eat(|kind| matches!(kind, TokenKind::Arrow)) else {
        return Err(ParseError::new(
            join_span(wait_span, signal_span),
            "`wait` binds the signal payload with `-> <name>`",
        ));
    };
    let bind = parse_binding(stream, arrow.span)?;
    let trailing = stream.end_line()?;
    Ok(WaitStmt {
        span: join_span(wait_span, bind.span),
        lead,
        trailing,
        signal,
        signal_span,
        timeout,
        bind,
    })
}

pub(super) fn parse_route_target(stream: &mut Stream) -> Result<RouteTarget, ParseError> {
    let (name, name_span) = stream.expect_name("a route target")?;
    if stream.peek_is(|kind| matches!(kind, TokenKind::LeftParen)) {
        stream.next();
        let payload = parse_route_payload(stream)?;
        let close = match &payload {
            ParsedPayload::Args(_, close) | ParsedPayload::Value(_, close) => *close,
        };
        let payload = match payload {
            ParsedPayload::Args(args, _) => crate::ast::RoutePayload::Args(args),
            ParsedPayload::Value(value, _) => crate::ast::RoutePayload::Value(value),
        };
        return Ok(RouteTarget {
            span: join_span(name_span, close),
            name,
            name_span,
            payload: Some(payload),
        });
    }
    Ok(RouteTarget {
        span: name_span,
        name,
        name_span,
        payload: None,
    })
}

enum ParsedPayload {
    Args(Vec<crate::ast::Arg>, Span),
    Value(crate::ast::Expr, Span),
}

/// Parse a route payload after its opening parenthesis: either named
/// construction fields (`route done(value: …)`) or one bare value expression
/// (`route out(verdict)` — the payload is the value itself). The two are
/// told apart by the `name:` lookahead.
fn parse_route_payload(stream: &mut Stream) -> Result<ParsedPayload, ParseError> {
    let name_shaped = stream.peek_is(|kind| {
        matches!(kind, TokenKind::Identifier(_))
            || matches!(kind, TokenKind::Keyword(keyword) if super::stream::soft_keyword(*keyword))
    });
    let named = stream.peek_is(|kind| matches!(kind, TokenKind::RightParen))
        || (name_shaped && stream.peek_second_is(|kind| matches!(kind, TokenKind::Colon)));
    if named {
        let (args, close) = parse_args(stream)?;
        return Ok(ParsedPayload::Args(args, close));
    }
    let value = super::exprs::parse_expr(stream)?;
    let close = stream.expect(
        &TokenKind::RightParen,
        "expected `)` to close the route payload",
    )?;
    Ok(ParsedPayload::Value(value, close.span))
}
