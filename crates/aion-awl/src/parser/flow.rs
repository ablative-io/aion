//! The rev-3 flow-shape grammar: `subflow` declarations, the per-item
//! region statements (`distribute`/`sequence` … `collect`), and the
//! `max N visits` step attribute. The checker owns every placement and
//! region rule; this module only builds the lossless tree.

use crate::ast::{
    Binding, CollectStmt, DeliveryVerb, DistributeStmt, DocLine, Lead, MaxVisits, Statement, Step,
    SubflowDecl, SubflowOutcome, join_span,
};
use crate::{Keyword, Span, TokenKind};

use super::ParseError;
use super::args::parse_binding;
use super::exprs::{expr_span, parse_expr};
use super::steps::parse_step;
use super::stream::{Stream, describe};
use super::types::parse_type_ref;
use super::workers::parse_params;

/// Parse a `subflow` declaration; the `subflow` keyword has been consumed.
///
/// Anatomy is the workflow's: typed inputs (the parenthesized parameters),
/// exactly one `outcome <name>: type <Type>` (no `route success|failure` —
/// the invocation site binds the payload), and the subflow's own steps.
pub(super) fn parse_subflow(
    stream: &mut Stream,
    lead: Vec<Lead>,
    docs: Vec<DocLine>,
    keyword_span: Span,
) -> Result<SubflowDecl, ParseError> {
    let (name, name_span) = stream.expect_name("a subflow name")?;
    let params = parse_params(stream, "subflow")?;
    let trailing = stream.end_line()?;
    let span = join_span(keyword_span, name_span);

    let mut outcome: Option<SubflowOutcome> = None;
    let mut steps: Vec<Step> = Vec::new();
    let outer_leads = stream.take_leads()?;
    if !stream.open_block() {
        return Err(ParseError::new(
            span,
            format!(
                "subflow `{name}` has no body — a subflow declares one \
                 `outcome <name>: type <Type>` and its steps in an indented block"
            ),
        ));
    }
    stream.push_back_leads(outer_leads);
    loop {
        let item_lead = stream.take_leads()?;
        if stream.at_item_block_end() {
            stream.push_back_leads(item_lead);
            break;
        }
        let item_docs = stream.take_docs();
        let Some(token) = stream.peek() else {
            stream.push_back_leads(item_lead);
            break;
        };
        let token_span = token.span;
        match &token.kind {
            TokenKind::Keyword(Keyword::Outcome) => {
                if let Some(doc) = item_docs.first() {
                    return Err(ParseError::new(
                        doc.span,
                        "`///` doc lines attach to declarations (types, fields, actions, steps), \
                         not a subflow outcome",
                    ));
                }
                stream.next();
                let declared = parse_subflow_outcome(stream, item_lead, token_span)?;
                if outcome.is_some() {
                    return Err(ParseError::new(
                        declared.name_span,
                        format!(
                            "subflow `{name}` declares a second outcome — a subflow has \
                             exactly one success outcome"
                        ),
                    ));
                }
                outcome = Some(declared);
            }
            TokenKind::Keyword(Keyword::Step) => {
                stream.next();
                steps.push(parse_step(stream, item_lead, item_docs, token_span)?);
            }
            other => {
                return Err(ParseError::new(
                    token_span,
                    format!(
                        "expected an `outcome` declaration or a `step` in the subflow body, \
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

    let Some(outcome) = outcome else {
        return Err(ParseError::new(
            span,
            format!(
                "subflow `{name}` declares no outcome — exactly one \
                 `outcome <name>: type <Type>` is required"
            ),
        ));
    };
    Ok(SubflowDecl {
        span,
        lead,
        docs,
        trailing,
        name,
        name_span,
        params,
        outcome,
        steps,
    })
}

/// Parse one subflow outcome after its `outcome` keyword has been consumed:
/// `outcome <name>: type <Type>` — no route direction.
fn parse_subflow_outcome(
    stream: &mut Stream,
    lead: Vec<Lead>,
    outcome_span: Span,
) -> Result<SubflowOutcome, ParseError> {
    let (name, name_span) = stream.expect_name("an outcome name")?;
    stream.expect(&TokenKind::Colon, "expected `:` after the outcome name")?;
    if stream
        .eat(|kind| matches!(kind, TokenKind::Keyword(Keyword::Type)))
        .is_none()
    {
        return Err(ParseError::new(
            stream.peek_span(),
            "a subflow outcome declares its payload type: `type <Type>`",
        ));
    }
    let ty = parse_type_ref(stream)?;
    if let Some(comma) = stream.eat(|kind| matches!(kind, TokenKind::Comma)) {
        return Err(ParseError::new(
            comma.span,
            "a subflow outcome carries no route — the invocation site binds the \
             payload and the enclosing flow routes on",
        ));
    }
    let trailing = stream.end_line()?;
    Ok(SubflowOutcome {
        span: join_span(outcome_span, name_span),
        lead,
        trailing,
        name,
        name_span,
        ty,
    })
}

/// Dispatch one region statement (`distribute`/`sequence`/`collect`) from
/// the shared statement parser; the keyword is at the cursor, unconsumed.
pub(super) fn parse_region_statement(
    stream: &mut Stream,
    lead: Vec<Lead>,
    docs: &[DocLine],
    keyword: crate::Keyword,
    span: Span,
) -> Result<Statement, ParseError> {
    use crate::Keyword as K;
    match keyword {
        K::Collect => {
            super::statements::reject_docs(docs, "a collect")?;
            stream.next();
            Ok(Statement::Collect(parse_collect(stream, lead, span)?))
        }
        verb => {
            let verb = if matches!(verb, K::Distribute) {
                DeliveryVerb::Distribute
            } else {
                DeliveryVerb::Sequence
            };
            super::statements::reject_docs(docs, "a region opener")?;
            stream.next();
            Ok(Statement::Distribute(parse_distribute(
                stream, lead, verb, span,
            )?))
        }
    }
}

/// Parse a `distribute <var> in <collection>` or `sequence <var> in
/// <collection>` statement; the verb keyword has been consumed.
pub(super) fn parse_distribute(
    stream: &mut Stream,
    lead: Vec<Lead>,
    verb: DeliveryVerb,
    verb_span: Span,
) -> Result<DistributeStmt, ParseError> {
    let (var, var_span) = stream.expect_name(&format!("the `{}` item name", verb.as_word()))?;
    match stream.peek() {
        Some(token) if matches!(&token.kind, TokenKind::Identifier(word) if word == "in") => {
            stream.next();
        }
        Some(token) => {
            return Err(ParseError::new(
                token.span,
                format!(
                    "expected `in` after the `{}` item name, found {}",
                    verb.as_word(),
                    describe(&token.kind)
                ),
            ));
        }
        None => {
            return Err(ParseError::new(
                stream.eof_span(),
                format!(
                    "expected `in` after the `{}` item name, found end of input",
                    verb.as_word()
                ),
            ));
        }
    }
    let collection = parse_expr(stream)?;
    let trailing = stream.end_line()?;
    Ok(DistributeStmt {
        span: join_span(verb_span, expr_span(&collection)),
        lead,
        trailing,
        verb,
        var,
        var_span,
        collection,
    })
}

/// Parse a `collect <binding>[?] -> <name>` statement; the `collect`
/// keyword has been consumed.
pub(super) fn parse_collect(
    stream: &mut Stream,
    lead: Vec<Lead>,
    collect_span: Span,
) -> Result<CollectStmt, ParseError> {
    let (binding, binding_span) = stream.expect_name("the binding `collect` gathers")?;
    let tolerant = stream
        .eat(|kind| matches!(kind, TokenKind::Question))
        .is_some();
    let Some(arrow) = stream.eat(|kind| matches!(kind, TokenKind::Arrow)) else {
        return Err(ParseError::new(
            join_span(collect_span, binding_span),
            "`collect` binds the gathered collection with `-> <name>`",
        ));
    };
    let bind: Binding = parse_binding(stream, arrow.span)?;
    let trailing = stream.end_line()?;
    Ok(CollectStmt {
        span: join_span(collect_span, bind.span),
        lead,
        trailing,
        binding,
        binding_span,
        tolerant,
        bind,
    })
}

/// Parse a `max <bound> visits` step attribute; the `max` keyword has been
/// consumed. The `visits` terminator is what distinguishes the attribute
/// from a loop's `max` tail (which never appears at step-block level).
pub(super) fn parse_max_visits(
    stream: &mut Stream,
    lead: Vec<Lead>,
    max_span: Span,
) -> Result<MaxVisits, ParseError> {
    let bound = parse_expr(stream)?;
    let Some(visits) = stream.eat(|kind| matches!(kind, TokenKind::Keyword(Keyword::Visits)))
    else {
        return Err(ParseError::new(
            join_span(max_span, expr_span(&bound)),
            "a step-level `max` is the re-entry bound and ends in `visits`: \
             `max <bound> visits` (a bounded `loop` writes its `max` inside the loop block)",
        ));
    };
    let trailing = stream.end_line()?;
    Ok(MaxVisits {
        span: join_span(max_span, visits.span),
        lead,
        trailing,
        bound,
    })
}

/// The one expression position where the `visits` keyword is a value: the
/// builtin visit counter, parsed as a reference the checker types.
pub(super) fn visits_expr(span: Span) -> crate::ast::Expr {
    crate::ast::Expr::Ref {
        span,
        name: "visits".to_owned(),
    }
}
