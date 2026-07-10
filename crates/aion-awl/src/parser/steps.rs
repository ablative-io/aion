//! Steps: headers with `after` dependencies, step blocks, loops, forks,
//! `on failure` blocks, and outcome clauses. Individual body statements
//! live in `statements`.

use crate::ast::{
    AfterRef, Binding, DocLine, ForkHeader, ForkStmt, Guard, JoinLine, Lead, LoopStmt, LoopTail,
    OnFailure, OutcomeClause, Statement, Step, join_span,
};
use crate::{Keyword, Span, TokenKind};

use super::ParseError;
use super::exprs::{expr_span, parse_binding, parse_expr};
use super::statements::{parse_route_target, parse_statement, reject_docs};
use super::stream::{Stream, describe};

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

pub(super) fn parse_loop(
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

pub(super) fn parse_fork(
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
