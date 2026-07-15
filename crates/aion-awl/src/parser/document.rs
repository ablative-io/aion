//! Document-level parsing: `//!` narration, the workflow header (inputs,
//! signals, outcomes), and top-level declaration dispatch.

use crate::ast::{
    DocLine, Document, InputDecl, Lead, OutcomeDecl, RouteDirection, SignalDecl,
    WorkflowTimeoutDecl, join_span,
};
use crate::{Keyword, Span, TokenKind, lex};

use super::ParseError;
use super::exprs::{expr_span, parse_expr};
use super::hints::gone_keyword_hint;
use super::steps::parse_step;
use super::stream::{Stream, describe};
use super::types::{parse_type_decl, parse_type_ref, type_ref_span};
use super::workers::{expect_duration, parse_child, parse_worker};

/// Parse a complete rev-2 AWL document.
///
/// # Errors
///
/// Returns the first lexical or syntactic error with a source-correct span.
pub fn parse(source: &str) -> Result<Document, ParseError> {
    let tokens = lex(source)?;
    let eof_line = source.split('\n').count();
    let mut stream = Stream::new(tokens, source.len(), eof_line);

    let narration = parse_narration(&mut stream)?;
    let lead = stream.take_leads()?;

    let workflow_token = match stream.peek() {
        Some(token) if matches!(token.kind, TokenKind::Keyword(Keyword::Workflow)) => {
            let span = token.span;
            stream.next();
            span
        }
        Some(token) => {
            return Err(ParseError::new(
                token.span,
                format!(
                    "expected the `workflow` header, found {}",
                    describe(&token.kind)
                ),
            ));
        }
        None => {
            return Err(ParseError::new(
                stream.eof_span(),
                "expected the `workflow` header, found end of input",
            ));
        }
    };
    let (name, name_span) = stream.expect_name("a workflow name")?;
    let trailing = stream.end_line()?;
    let workflow_span = join_span(workflow_token, name_span);

    let mut document = Document {
        span: Span {
            start: 0,
            end: source.len(),
            line: 1,
            column: 1,
        },
        narration,
        lead,
        name,
        name_span,
        trailing,
        timeout: None,
        inputs: Vec::new(),
        signals: Vec::new(),
        outcomes: Vec::new(),
        consts: Vec::new(),
        types: Vec::new(),
        workers: Vec::new(),
        children: Vec::new(),
        steps: Vec::new(),
        epilogue: Vec::new(),
    };

    parse_header_block(&mut stream, &mut document)?;
    if document.outcomes.is_empty() {
        return Err(ParseError::new(
            workflow_span,
            format!(
                "workflow `{}` declares no outcomes; at least one \
                 `outcome <name>: type <Type>, route success|failure` is required",
                document.name
            ),
        ));
    }
    parse_declarations(&mut stream, &mut document)?;
    document.epilogue = stream.take_leads()?;
    if let Some(token) = stream.peek() {
        return Err(ParseError::new(
            token.span,
            format!("expected end of document, found {}", describe(&token.kind)),
        ));
    }
    Ok(document)
}

/// Parse the mandatory `//!` narration lines at the top of the document.
fn parse_narration(stream: &mut Stream) -> Result<Vec<DocLine>, ParseError> {
    let mut narration = Vec::new();
    while let Some(token) = stream.peek() {
        let span = token.span;
        let TokenKind::DocHeader(text) = &token.kind else {
            break;
        };
        let text = text.clone();
        stream.next();
        narration.push(DocLine { span, text });
        stream.eat(|kind| matches!(kind, TokenKind::Newline));
    }
    if narration.is_empty() {
        return Err(ParseError::new(
            stream.peek_span(),
            "a workflow document opens with `//!` narration — one or more lines \
             before the `workflow` header",
        ));
    }
    Ok(narration)
}

/// Parse the indented header block: `timeout`, `input`, `signal`, and `outcome`
/// declarations in any order (the printer canonicalizes the order).
fn parse_header_block(stream: &mut Stream, document: &mut Document) -> Result<(), ParseError> {
    let leads = stream.take_leads()?;
    if !stream.open_block() {
        stream.push_back_leads(leads);
        return Ok(());
    }
    stream.push_back_leads(leads);
    loop {
        let lead = stream.take_leads()?;
        if stream.at_item_block_end() {
            stream.push_back_leads(lead);
            break;
        }
        let Some(token) = stream.peek() else {
            stream.push_back_leads(lead);
            break;
        };
        let span = token.span;
        match &token.kind {
            TokenKind::Keyword(Keyword::Timeout) => {
                stream.next();
                if document.timeout.is_some() {
                    return Err(ParseError::new(
                        span,
                        "duplicate workflow `timeout` declaration",
                    ));
                }
                let negative = stream
                    .eat(|kind| matches!(kind, TokenKind::Minus))
                    .is_some();
                let duration = expect_duration(
                    stream,
                    span,
                    "workflow `timeout` needs a duration (`30s`, `5m`, `3h`, `2d`)",
                )?;
                let trailing = stream.end_line()?;
                document.timeout = Some(WorkflowTimeoutDecl {
                    span: join_span(span, duration.span),
                    lead,
                    trailing,
                    duration,
                    negative,
                });
            }
            TokenKind::Keyword(Keyword::Input) => {
                stream.next();
                let decl = parse_io_decl(stream, "input")?;
                document.inputs.push(InputDecl {
                    span: join_span(span, decl.type_span),
                    lead,
                    trailing: decl.trailing,
                    name: decl.name,
                    name_span: decl.name_span,
                    ty: decl.ty,
                });
            }
            TokenKind::Keyword(Keyword::Signal) => {
                stream.next();
                let decl = parse_io_decl(stream, "signal")?;
                document.signals.push(SignalDecl {
                    span: join_span(span, decl.type_span),
                    lead,
                    trailing: decl.trailing,
                    name: decl.name,
                    name_span: decl.name_span,
                    ty: decl.ty,
                });
            }
            TokenKind::Keyword(Keyword::Outcome) => {
                stream.next();
                document
                    .outcomes
                    .push(parse_outcome_decl(stream, lead, span)?);
            }
            TokenKind::Identifier(word) => {
                let message = gone_keyword_hint(word).unwrap_or_else(|| {
                    format!(
                        "expected `timeout`, `input`, `signal`, or `outcome` in the workflow header, \
                         found `{word}`"
                    )
                });
                return Err(ParseError::new(span, message));
            }
            other => {
                return Err(ParseError::new(
                    span,
                    format!(
                        "expected `timeout`, `input`, `signal`, or `outcome` in the workflow header, \
                         found {}",
                        describe(other)
                    ),
                ));
            }
        }
    }
    let stray = stream.take_leads()?;
    stream.consume_block_dedent();
    stream.push_back_leads(stray);
    Ok(())
}

struct IoDecl {
    name: String,
    name_span: Span,
    ty: crate::ast::TypeRef,
    type_span: Span,
    trailing: Option<crate::ast::Comment>,
}

fn parse_io_decl(stream: &mut Stream, what: &str) -> Result<IoDecl, ParseError> {
    let (name, name_span) = stream.expect_name(&format!("an {what} name"))?;
    if stream
        .eat(|kind| matches!(kind, TokenKind::Colon))
        .is_none()
    {
        return Err(ParseError::new(
            name_span,
            format!("an `{what}` declares its contract as `{what} <name>: <Type>`"),
        ));
    }
    let ty = parse_type_ref(stream)?;
    let type_span = type_ref_span(&ty);
    let trailing = stream.end_line()?;
    Ok(IoDecl {
        name,
        name_span,
        ty,
        type_span,
        trailing,
    })
}

fn parse_outcome_decl(
    stream: &mut Stream,
    lead: Vec<Lead>,
    outcome_span: Span,
) -> Result<OutcomeDecl, ParseError> {
    let (name, name_span) = stream.expect_name("an outcome name")?;
    stream.expect(&TokenKind::Colon, "expected `:` after the outcome name")?;
    if stream
        .eat(|kind| matches!(kind, TokenKind::Keyword(Keyword::Type)))
        .is_none()
    {
        return Err(ParseError::new(
            stream.peek_span(),
            "a workflow outcome declares its payload type: `type <Type>`",
        ));
    }
    let ty = parse_type_ref(stream)?;
    stream.expect(
        &TokenKind::Comma,
        "expected `,` between the outcome type and its route",
    )?;
    if stream
        .eat(|kind| matches!(kind, TokenKind::Keyword(Keyword::Route)))
        .is_none()
    {
        return Err(ParseError::new(
            stream.peek_span(),
            "a workflow outcome maps to a terminal status: `route success` or `route failure`",
        ));
    }
    let direction = match stream.peek() {
        Some(token) => {
            let span = token.span;
            match &token.kind {
                TokenKind::Keyword(Keyword::Success) => {
                    stream.next();
                    RouteDirection::Success
                }
                TokenKind::Keyword(Keyword::Failure) => {
                    stream.next();
                    RouteDirection::Failure
                }
                other => {
                    return Err(ParseError::new(
                        span,
                        format!(
                            "a workflow outcome routes to `success` or `failure`, not {}",
                            describe(other)
                        ),
                    ));
                }
            }
        }
        None => {
            return Err(ParseError::new(
                stream.eof_span(),
                "a workflow outcome routes to `success` or `failure`",
            ));
        }
    };
    let trailing = stream.end_line()?;
    Ok(OutcomeDecl {
        span: join_span(outcome_span, name_span),
        lead,
        trailing,
        name,
        name_span,
        ty,
        direction,
    })
}

/// Parse the top-level declarations after the header: consts, types,
/// workers, children, and steps, in any interleaving (the printer
/// canonicalizes the grammar's order).
fn parse_declarations(stream: &mut Stream, document: &mut Document) -> Result<(), ParseError> {
    loop {
        let lead = stream.take_leads()?;
        let docs = stream.take_docs();
        let Some(token) = stream.peek() else {
            if let Some(doc) = docs.first() {
                return Err(ParseError::new(
                    doc.span,
                    "`///` doc lines attach to the declaration that follows; nothing follows",
                ));
            }
            stream.push_back_leads(lead);
            return Ok(());
        };
        let span = token.span;
        match &token.kind {
            TokenKind::Keyword(Keyword::Const) => {
                stream.next();
                document
                    .consts
                    .push(parse_const_decl(stream, lead, docs, span)?);
            }
            TokenKind::Keyword(Keyword::Type) => {
                stream.next();
                document
                    .types
                    .push(parse_type_decl(stream, lead, docs, span)?);
            }
            TokenKind::Keyword(Keyword::Worker) => {
                stream.next();
                document
                    .workers
                    .push(parse_worker(stream, lead, docs, span)?);
            }
            TokenKind::Keyword(Keyword::Child) => {
                stream.next();
                document
                    .children
                    .push(parse_child(stream, lead, docs, span)?);
            }
            TokenKind::Keyword(Keyword::Step) => {
                stream.next();
                document.steps.push(parse_step(stream, lead, docs, span)?);
            }
            TokenKind::Identifier(word) => {
                let message = gone_keyword_hint(word).unwrap_or_else(|| {
                    format!(
                        "expected a `const`, `type`, `worker`, `child`, or `step` declaration, \
                         found `{word}`"
                    )
                });
                return Err(ParseError::new(span, message));
            }
            other => {
                return Err(ParseError::new(
                    span,
                    format!(
                        "expected a `const`, `type`, `worker`, `child`, or `step` declaration, \
                         found {}",
                        describe(other)
                    ),
                ));
            }
        }
    }
}

/// Parse one `const name = <value>` declaration after the `const` keyword
/// (at `const_span`) has been consumed. The value is a general expression
/// here; the checker restricts it to compile-time forms.
fn parse_const_decl(
    stream: &mut Stream,
    lead: Vec<Lead>,
    docs: Vec<DocLine>,
    const_span: Span,
) -> Result<crate::ast::ConstDecl, ParseError> {
    let (name, name_span) = stream.expect_name("a const name")?;
    if stream
        .eat(|kind| matches!(kind, TokenKind::Equal))
        .is_none()
    {
        return Err(ParseError::new(
            name_span,
            format!("a `const` declares its value with `=`: `const {name} = <value>`"),
        ));
    }
    let value = parse_expr(stream)?;
    let trailing = stream.end_line()?;
    Ok(crate::ast::ConstDecl {
        span: join_span(const_span, expr_span(&value)),
        lead,
        docs,
        trailing,
        name,
        name_span,
        value,
    })
}
