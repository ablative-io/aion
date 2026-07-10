//! Steps: headers with `after` dependencies, body statements (calls, pipes,
//! waits, forks, loops, substeps), `on failure` blocks, and outcome clauses.

use crate::ast::{
    AfterRef, Binding, Call, CallStmt, CombinatorCall, CombinatorKind, DocLine, ForkHeader,
    ForkStmt, Guard, JoinLine, Lead, LoopStmt, LoopTail, OnFailure, OutcomeClause, PipeEnd,
    PipeStage, PipeStmt, RouteStmt, RouteTarget, SleepStmt, SpawnStmt, Statement, Step, WaitStmt,
    join_span,
};
use crate::{Keyword, Span, TokenKind};

use super::ParseError;
use super::exprs::{expr_span, parse_args, parse_binding, parse_expr};
use super::stream::{Stream, describe, gone_keyword_hint};
use super::workers::{expect_duration, parse_config_block};

/// Parse a `step` declaration; the `step` keyword has been consumed.
pub(super) fn parse_step(
    stream: &mut Stream,
    lead: Vec<Lead>,
    docs: Vec<DocLine>,
    keyword_span: Span,
) -> Result<Step, ParseError> {
    let (name, name_span) = stream.expect_name("a step name")?;
    let mut after = Vec::new();
    if stream
        .eat(|kind| matches!(kind, TokenKind::Keyword(Keyword::After)))
        .is_some()
    {
        loop {
            let (dep, dep_span) = stream.expect_name("a step name after `after`")?;
            after.push(AfterRef {
                span: dep_span,
                name: dep,
            });
            if stream
                .eat(|kind| matches!(kind, TokenKind::Comma))
                .is_none()
            {
                break;
            }
        }
    }
    let span = join_span(keyword_span, name_span);
    let trailing = stream.end_line()?;

    let mut body = Vec::new();
    let mut on_failure = None;
    let mut outcomes = Vec::new();
    let outer_leads = stream.take_leads()?;
    if stream.open_block() {
        stream.push_back_leads(outer_leads);
        parse_step_block(stream, &mut body, &mut on_failure, &mut outcomes)?;
        let stray = stream.take_leads()?;
        stream.consume_block_dedent();
        stream.push_back_leads(stray);
    } else {
        stream.push_back_leads(outer_leads);
    }

    Ok(Step {
        span,
        lead,
        docs,
        trailing,
        name,
        name_span,
        after,
        body,
        on_failure,
        outcomes,
    })
}

/// Parse the contents of a step's indented block until its dedent.
fn parse_step_block(
    stream: &mut Stream,
    body: &mut Vec<Statement>,
    on_failure: &mut Option<OnFailure>,
    outcomes: &mut Vec<OutcomeClause>,
) -> Result<(), ParseError> {
    loop {
        let lead = stream.take_leads()?;
        if stream.at_item_block_end() {
            stream.push_back_leads(lead);
            return Ok(());
        }
        let docs = stream.take_docs();
        let Some(token) = stream.peek() else {
            stream.push_back_leads(lead);
            return Ok(());
        };
        let token_span = token.span;
        match &token.kind {
            TokenKind::Keyword(Keyword::Outcome) => {
                reject_docs(&docs, "an outcome clause")?;
                stream.next();
                outcomes.push(parse_outcome_clause(stream, lead, token_span)?);
            }
            TokenKind::Keyword(Keyword::On) => {
                reject_docs(&docs, "an `on failure` block")?;
                stream.next();
                if on_failure.is_some() {
                    return Err(ParseError::new(
                        token_span,
                        "a step declares at most one `on failure` block",
                    ));
                }
                *on_failure = Some(parse_on_failure(stream, lead, token_span)?);
            }
            _ => {
                body.push(parse_statement(stream, lead, docs)?);
            }
        }
    }
}

fn reject_docs(docs: &[DocLine], what: &str) -> Result<(), ParseError> {
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
fn parse_statement(
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
        other => Err(ParseError::new(
            span,
            format!("expected a statement, found {}", describe(other)),
        )),
    }
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
            keyword @ (Keyword::Filter | Keyword::Map | Keyword::Sort | Keyword::Count),
        ) => {
            let kind = match keyword {
                Keyword::Filter => CombinatorKind::Filter,
                Keyword::Map => CombinatorKind::Map,
                Keyword::Sort => CombinatorKind::Sort,
                _ => CombinatorKind::Count,
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

fn parse_loop(
    stream: &mut Stream,
    lead: Vec<Lead>,
    loop_span: Span,
) -> Result<LoopStmt, ParseError> {
    let (var, var_span) = stream.expect_name("the loop's threaded name")?;
    if stream
        .eat(|kind| matches!(kind, TokenKind::Equal))
        .is_none()
    {
        return Err(ParseError::new(
            stream.peek_span(),
            format!("`loop` threads one value between iterations: write `loop {var} = <seed>`"),
        ));
    }
    let seed = parse_expr(stream)?;
    let counter = if let Some(counting) =
        stream.eat(|kind| matches!(kind, TokenKind::Keyword(Keyword::Counting)))
    {
        match stream.peek() {
            Some(token) if matches!(token.kind, TokenKind::Identifier(_)) => {
                let (name, span) = stream.expect_name("a counter name")?;
                Some(Binding { span, name })
            }
            _ => {
                return Err(ParseError::new(
                    counting.span,
                    "`counting` binds the language-owned counter: write `counting <name>`",
                ));
            }
        }
    } else {
        None
    };
    let trailing = stream.end_line()?;

    let mut body = Vec::new();
    let mut until = None;
    let mut max = None;
    let block_leads = stream.take_leads()?;
    if stream.open_block() {
        stream.push_back_leads(block_leads);
        loop {
            let stmt_lead = stream.take_leads()?;
            if stream.at_item_block_end() {
                stream.push_back_leads(stmt_lead);
                break;
            }
            match stream.peek().map(|token| (token.span, token.kind.clone())) {
                Some((span, TokenKind::Keyword(Keyword::Until))) => {
                    stream.next();
                    let expr = parse_expr(stream)?;
                    let tail_trailing = stream.end_line()?;
                    until = Some(LoopTail {
                        span: join_span(span, expr_span(&expr)),
                        lead: stmt_lead,
                        trailing: tail_trailing,
                        expr,
                    });
                }
                Some((span, TokenKind::Keyword(Keyword::Max))) => {
                    stream.next();
                    let expr = parse_expr(stream)?;
                    let tail_trailing = stream.end_line()?;
                    max = Some(LoopTail {
                        span: join_span(span, expr_span(&expr)),
                        lead: stmt_lead,
                        trailing: tail_trailing,
                        expr,
                    });
                }
                _ => {
                    let docs = stream.take_docs();
                    body.push(parse_statement(stream, stmt_lead, docs)?);
                }
            }
        }
        let stray = stream.take_leads()?;
        stream.consume_block_dedent();
        stream.push_back_leads(stray);
    } else {
        stream.push_back_leads(block_leads);
    }

    Ok(LoopStmt {
        span: loop_span,
        lead,
        trailing,
        var,
        var_span,
        seed,
        counter,
        body,
        until,
        max,
    })
}

fn parse_fork(
    stream: &mut Stream,
    lead: Vec<Lead>,
    fork_span: Span,
) -> Result<ForkStmt, ParseError> {
    let header = if stream
        .peek_is(|kind| matches!(kind, TokenKind::Newline | TokenKind::Comment(_)))
    {
        ForkHeader::Named
    } else {
        let (var, var_span) = stream.expect_name("the fork item name")?;
        match stream.peek() {
            Some(token) if matches!(&token.kind, TokenKind::Identifier(word) if word == "in") => {
                stream.next();
            }
            Some(token) => {
                return Err(ParseError::new(
                    token.span,
                    format!(
                        "expected `in` after the fork item name, found {}",
                        describe(&token.kind)
                    ),
                ));
            }
            None => {
                return Err(ParseError::new(
                    stream.eof_span(),
                    "expected `in` after the fork item name, found end of input",
                ));
            }
        }
        let collection = parse_expr(stream)?;
        let sequential = stream
            .eat(|kind| matches!(kind, TokenKind::Keyword(Keyword::Sequential)))
            .is_some();
        ForkHeader::Collection {
            var,
            var_span,
            collection,
            sequential,
        }
    };
    let trailing = stream.end_line()?;

    let mut body = Vec::new();
    let block_leads = stream.take_leads()?;
    if !stream.open_block() {
        return Err(ParseError::new(
            fork_span,
            "expected an indented branch block under `fork`".to_owned(),
        ));
    }
    stream.push_back_leads(block_leads);
    loop {
        let stmt_lead = stream.take_leads()?;
        if stream.at_item_block_end() {
            stream.push_back_leads(stmt_lead);
            break;
        }
        let docs = stream.take_docs();
        body.push(parse_statement(stream, stmt_lead, docs)?);
    }
    let stray = stream.take_leads()?;
    stream.consume_block_dedent();
    stream.push_back_leads(stray);

    let join_lead = stream.take_leads()?;
    let join_token = stream.expect(
        &TokenKind::Keyword(Keyword::Join),
        "expected `join` to close the fork block",
    )?;
    let bind = match stream.eat(|kind| matches!(kind, TokenKind::Arrow)) {
        Some(arrow) => Some(parse_binding(stream, arrow.span)?),
        None => None,
    };
    let join_trailing = stream.end_line()?;

    Ok(ForkStmt {
        span: fork_span,
        lead,
        trailing,
        header,
        body,
        join: JoinLine {
            span: join_token.span,
            lead: join_lead,
            trailing: join_trailing,
            bind,
        },
    })
}

fn parse_on_failure(
    stream: &mut Stream,
    lead: Vec<Lead>,
    on_span: Span,
) -> Result<OnFailure, ParseError> {
    stream.expect(
        &TokenKind::Keyword(Keyword::Failure),
        "expected `failure` after `on`",
    )?;
    let trailing = stream.end_line()?;
    let mut body = Vec::new();
    let block_leads = stream.take_leads()?;
    if !stream.open_block() {
        return Err(ParseError::new(
            on_span,
            "expected an indented block under `on failure`".to_owned(),
        ));
    }
    stream.push_back_leads(block_leads);
    loop {
        let stmt_lead = stream.take_leads()?;
        if stream.at_item_block_end() {
            stream.push_back_leads(stmt_lead);
            break;
        }
        let docs = stream.take_docs();
        body.push(parse_statement(stream, stmt_lead, docs)?);
    }
    let stray = stream.take_leads()?;
    stream.consume_block_dedent();
    stream.push_back_leads(stray);
    Ok(OnFailure {
        span: on_span,
        lead,
        trailing,
        body,
    })
}

/// Parse an outcome clause after its `outcome` keyword has been consumed.
fn parse_outcome_clause(
    stream: &mut Stream,
    lead: Vec<Lead>,
    outcome_span: Span,
) -> Result<OutcomeClause, ParseError> {
    let (name, name_span) = stream.expect_name("an outcome arm name")?;
    stream.expect(&TokenKind::Colon, "expected `:` after the outcome arm name")?;
    let guard = match stream.peek() {
        Some(token) => {
            let span = token.span;
            match token.kind {
                TokenKind::Keyword(Keyword::When) => {
                    stream.next();
                    let expr = parse_expr(stream)?;
                    Guard::When {
                        span: join_span(span, expr_span(&expr)),
                        expr,
                    }
                }
                TokenKind::Keyword(Keyword::Otherwise) => {
                    stream.next();
                    Guard::Otherwise { span }
                }
                ref other => {
                    return Err(ParseError::new(
                        span,
                        format!(
                            "expected `when <condition>` or `otherwise`, found {}",
                            describe(other)
                        ),
                    ));
                }
            }
        }
        None => {
            return Err(ParseError::new(
                stream.eof_span(),
                "expected `when <condition>` or `otherwise`, found end of input",
            ));
        }
    };
    let clause_span = join_span(outcome_span, name_span);
    if stream
        .eat(|kind| matches!(kind, TokenKind::Comma))
        .is_none()
    {
        return Err(ParseError::new(
            clause_span,
            "an outcome arm must say where control goes: `, route <target>`",
        ));
    }
    // Canonical two-line break: the route may continue on the next line,
    // one level deeper.
    let mut continued = false;
    if stream.peek_is(|kind| matches!(kind, TokenKind::Newline)) {
        stream.next();
        stream.expect_indent(
            clause_span,
            "an outcome arm must say where control goes: `route <target>`",
        )?;
        continued = true;
    }
    if stream
        .eat(|kind| matches!(kind, TokenKind::Keyword(Keyword::Route)))
        .is_none()
    {
        return Err(ParseError::new(
            clause_span,
            "an outcome arm must say where control goes: `route <target>`",
        ));
    }
    let route = parse_route_target(stream)?;
    let trailing = stream.end_line()?;
    if continued {
        let stray = stream.take_leads()?;
        stream.consume_block_dedent();
        stream.push_back_leads(stray);
    }
    Ok(OutcomeClause {
        span: clause_span,
        lead,
        trailing,
        name,
        name_span,
        guard,
        route,
    })
}

fn parse_route_target(stream: &mut Stream) -> Result<RouteTarget, ParseError> {
    let (name, name_span) = stream.expect_name("a route target")?;
    let payload = if stream.peek_is(|kind| matches!(kind, TokenKind::LeftParen)) {
        stream.next();
        let (args, close) = parse_args(stream)?;
        return Ok(RouteTarget {
            span: join_span(name_span, close),
            name,
            name_span,
            payload: Some(args),
        });
    } else {
        None
    };
    Ok(RouteTarget {
        span: name_span,
        name,
        name_span,
        payload,
    })
}
