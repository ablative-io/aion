//! Expression parsing: precedence-climbing over the fixed rev-2 vocabulary
//! (`or`/`and`/`not`, comparisons, `is` predicates, string `+`, field
//! access, literal-only indexing, collection predicates, literals, record
//! construction, and the `workflow` builtin namespace).

use crate::ast::{BinaryOp, DurationLiteral, Expr, PredicateKind, Quantifier, join_span};
use crate::{Keyword, TokenKind};

use super::ParseError;
use super::args::parse_args;
use super::stream::{Stream, describe};

/// Parse a full expression (lowest precedence: `or`).
pub(super) fn parse_expr(stream: &mut Stream) -> Result<Expr, ParseError> {
    parse_or(stream)
}

fn parse_or(stream: &mut Stream) -> Result<Expr, ParseError> {
    let mut left = parse_and(stream)?;
    while stream
        .eat(|kind| matches!(kind, TokenKind::Keyword(Keyword::Or)))
        .is_some()
    {
        let right = parse_and(stream)?;
        left = binary(left, BinaryOp::Or, right);
    }
    Ok(left)
}

fn parse_and(stream: &mut Stream) -> Result<Expr, ParseError> {
    let mut left = parse_not(stream)?;
    while stream
        .eat(|kind| matches!(kind, TokenKind::Keyword(Keyword::And)))
        .is_some()
    {
        let right = parse_not(stream)?;
        left = binary(left, BinaryOp::And, right);
    }
    Ok(left)
}

fn parse_not(stream: &mut Stream) -> Result<Expr, ParseError> {
    if let Some(token) = stream.eat(|kind| matches!(kind, TokenKind::Keyword(Keyword::Not))) {
        let operand = parse_not(stream)?;
        let span = join_span(token.span, expr_span(&operand));
        return Ok(Expr::Not {
            span,
            expr: Box::new(operand),
        });
    }
    parse_comparison(stream)
}

fn parse_comparison(stream: &mut Stream) -> Result<Expr, ParseError> {
    let left = parse_concat(stream)?;
    if let Some(op) = stream.peek().and_then(|token| comparison_op(&token.kind)) {
        stream.next();
        let right = parse_concat(stream)?;
        return Ok(binary(left, op, right));
    }
    if stream
        .eat(|kind| matches!(kind, TokenKind::Keyword(Keyword::Is)))
        .is_some()
    {
        return parse_predicate(stream, left);
    }
    Ok(left)
}

fn parse_predicate(stream: &mut Stream, subject: Expr) -> Result<Expr, ParseError> {
    let (kind, token) = match stream.peek() {
        Some(token) => {
            let kind = match token.kind {
                TokenKind::Keyword(Keyword::Empty) => PredicateKind::Empty,
                TokenKind::Keyword(Keyword::Present) => PredicateKind::Present,
                TokenKind::Keyword(Keyword::Absent) => PredicateKind::Absent,
                ref other => {
                    return Err(ParseError::new(
                        token.span,
                        format!(
                            "expected `empty`, `present`, or `absent` after `is`, found {}",
                            describe(other)
                        ),
                    ));
                }
            };
            (kind, token.span)
        }
        None => {
            return Err(ParseError::new(
                stream.eof_span(),
                "expected `empty`, `present`, or `absent` after `is`",
            ));
        }
    };
    stream.next();
    let span = join_span(expr_span(&subject), token);
    Ok(Expr::Predicate {
        span,
        subject: Box::new(subject),
        kind,
    })
}

fn parse_concat(stream: &mut Stream) -> Result<Expr, ParseError> {
    let mut left = parse_postfix(stream)?;
    while stream.eat(|kind| matches!(kind, TokenKind::Plus)).is_some() {
        let right = parse_postfix(stream)?;
        left = binary(left, BinaryOp::Concat, right);
    }
    Ok(left)
}

fn parse_postfix(stream: &mut Stream) -> Result<Expr, ParseError> {
    let mut expr = parse_primary(stream)?;
    loop {
        if let Some(token) = stream.peek() {
            let span = token.span;
            match &token.kind {
                TokenKind::FieldAccessor(name) => {
                    let name = name.clone();
                    stream.next();
                    let full = join_span(expr_span(&expr), span);
                    expr = Expr::Field {
                        span: full,
                        base: Box::new(expr),
                        name,
                        name_span: span,
                    };
                    continue;
                }
                TokenKind::LeftBracket => {
                    stream.next();
                    expr = parse_index(stream, expr)?;
                    continue;
                }
                _ => {}
            }
            if matches!(token.kind, TokenKind::Pipe)
                && let Some(next) = stream.peek_second()
                && let TokenKind::Keyword(keyword @ (Keyword::Any | Keyword::All)) = next.kind
            {
                let quantifier = if matches!(keyword, Keyword::Any) {
                    Quantifier::Any
                } else {
                    Quantifier::All
                };
                stream.next();
                stream.next();
                stream.expect(
                    &TokenKind::LeftParen,
                    "expected `(` after the collection predicate",
                )?;
                let predicate = parse_expr(stream)?;
                let close = stream.expect(
                    &TokenKind::RightParen,
                    "expected `)` to close the collection predicate",
                )?;
                expr = Expr::CollectionPredicate {
                    span: join_span(expr_span(&expr), close.span),
                    collection: Box::new(expr),
                    quantifier,
                    predicate: Box::new(predicate),
                };
                continue;
            }
        }
        return Ok(expr);
    }
}

fn parse_index(stream: &mut Stream, base: Expr) -> Result<Expr, ParseError> {
    let (index, index_span) = match stream.peek() {
        Some(token) => match token.kind {
            TokenKind::Integer(value) => (value, token.span),
            ref other => {
                return Err(ParseError::new(
                    token.span,
                    format!(
                        "indexing is literal-only (`items[0]`): a computed index of {} is not writable",
                        describe(other)
                    ),
                ));
            }
        },
        None => {
            return Err(ParseError::new(
                stream.eof_span(),
                "indexing is literal-only (`items[0]`); expected an integer literal",
            ));
        }
    };
    stream.next();
    let close = stream.expect(&TokenKind::RightBracket, "expected `]` after the index")?;
    let span = join_span(expr_span(&base), close.span);
    Ok(Expr::Index {
        span,
        base: Box::new(base),
        index,
        index_span,
    })
}

fn parse_primary(stream: &mut Stream) -> Result<Expr, ParseError> {
    let Some(token) = stream.peek() else {
        return Err(ParseError::new(
            stream.eof_span(),
            "expected an expression, found end of input",
        ));
    };
    let span = token.span;
    match &token.kind {
        TokenKind::String(value) => {
            let value = value.clone();
            stream.next();
            Ok(Expr::String { span, value })
        }
        TokenKind::RawString(value) => {
            let value = value.clone();
            stream.next();
            Ok(Expr::RawString { span, value })
        }
        TokenKind::Keyword(Keyword::Json) => {
            stream.next();
            parse_json_literal(stream, span)
        }
        TokenKind::Keyword(Keyword::Schema) => {
            stream.next();
            parse_schema_of(stream, span)
        }
        TokenKind::Integer(value) => {
            let value = *value;
            stream.next();
            Ok(Expr::Int { span, value })
        }
        TokenKind::Float(text) => {
            let value = text.clone();
            stream.next();
            Ok(Expr::Float { span, value })
        }
        TokenKind::Keyword(Keyword::True) => {
            stream.next();
            Ok(Expr::Bool { span, value: true })
        }
        TokenKind::Keyword(Keyword::False) => {
            stream.next();
            Ok(Expr::Bool { span, value: false })
        }
        TokenKind::Duration { magnitude, unit } => {
            let literal = DurationLiteral {
                span,
                magnitude: *magnitude,
                unit: *unit,
            };
            stream.next();
            Ok(Expr::Duration(literal))
        }
        TokenKind::LeftBracket => {
            stream.next();
            parse_list(stream, span)
        }
        TokenKind::Identifier(name) => {
            let name = name.clone();
            stream.next();
            Ok(Expr::Ref { span, name })
        }
        TokenKind::Keyword(Keyword::Workflow) => {
            stream.next();
            Ok(Expr::Workflow { span })
        }
        TokenKind::TypeIdentifier(name) => {
            let name = name.clone();
            stream.next();
            if stream.peek_is(|kind| matches!(kind, TokenKind::LeftParen)) {
                stream.next();
                let (args, close) = parse_args(stream)?;
                Ok(Expr::Record {
                    span: join_span(span, close),
                    name,
                    name_span: span,
                    args,
                })
            } else {
                Ok(Expr::Variant { span, name })
            }
        }
        TokenKind::FieldAccessor(name) => {
            let name = name.clone();
            stream.next();
            Ok(Expr::Accessor { span, name })
        }
        other => Err(ParseError::new(
            span,
            format!("expected an expression, found {}", describe(other)),
        )),
    }
}

/// Parse the body of a `json { … }` literal after the `json` keyword (at
/// `json_span`) has been consumed. The lexer already captured the balanced
/// braces as one verbatim token.
fn parse_json_literal(stream: &mut Stream, json_span: crate::Span) -> Result<Expr, ParseError> {
    match stream.peek() {
        Some(token) => {
            let body_span = token.span;
            if let TokenKind::JsonBody(body) = &token.kind {
                let body = body.clone();
                stream.next();
                Ok(Expr::Json {
                    span: join_span(json_span, body_span),
                    body,
                    body_span,
                })
            } else {
                Err(ParseError::new(
                    body_span,
                    format!(
                        "a `json` literal opens its body with `{{`, found {}",
                        describe(&token.kind)
                    ),
                ))
            }
        }
        None => Err(ParseError::new(
            stream.eof_span(),
            "a `json` literal opens its body with `{`, found end of input",
        )),
    }
}

/// Parse `schema of <Type>` after the `schema` keyword (at `schema_span`)
/// has been consumed.
fn parse_schema_of(stream: &mut Stream, schema_span: crate::Span) -> Result<Expr, ParseError> {
    if stream
        .eat(|kind| matches!(kind, TokenKind::Keyword(Keyword::Of)))
        .is_none()
    {
        return Err(ParseError::new(
            stream.peek_span(),
            "`schema` in an expression takes the form `schema of <Type>`",
        ));
    }
    match stream.peek() {
        Some(token) => {
            let name_span = token.span;
            if let TokenKind::TypeIdentifier(name) = &token.kind {
                let name = name.clone();
                stream.next();
                Ok(Expr::SchemaOf {
                    span: join_span(schema_span, name_span),
                    name,
                    name_span,
                })
            } else {
                Err(ParseError::new(
                    name_span,
                    format!(
                        "`schema of` names a TitleCase type, found {}",
                        describe(&token.kind)
                    ),
                ))
            }
        }
        None => Err(ParseError::new(
            stream.eof_span(),
            "`schema of` names a TitleCase type, found end of input",
        )),
    }
}

fn parse_list(stream: &mut Stream, open: crate::Span) -> Result<Expr, ParseError> {
    let mut items = Vec::new();
    loop {
        if let Some(close) = stream.eat(|kind| matches!(kind, TokenKind::RightBracket)) {
            return Ok(Expr::List {
                span: join_span(open, close.span),
                items,
            });
        }
        items.push(parse_expr(stream)?);
        if stream
            .eat(|kind| matches!(kind, TokenKind::Comma))
            .is_some()
        {
            continue;
        }
        let close = stream.expect(
            &TokenKind::RightBracket,
            "expected `]` to close the list literal",
        )?;
        return Ok(Expr::List {
            span: join_span(open, close.span),
            items,
        });
    }
}

fn comparison_op(kind: &TokenKind) -> Option<BinaryOp> {
    match kind {
        TokenKind::EqualEqual => Some(BinaryOp::Eq),
        TokenKind::BangEqual => Some(BinaryOp::Ne),
        TokenKind::Less => Some(BinaryOp::Lt),
        TokenKind::LessEqual => Some(BinaryOp::Le),
        TokenKind::Greater => Some(BinaryOp::Gt),
        TokenKind::GreaterEqual => Some(BinaryOp::Ge),
        _ => None,
    }
}

fn binary(left: Expr, op: BinaryOp, right: Expr) -> Expr {
    let span = join_span(expr_span(&left), expr_span(&right));
    Expr::Binary {
        span,
        left: Box::new(left),
        op,
        right: Box::new(right),
    }
}

/// The source span of an expression node.
pub(super) fn expr_span(expr: &Expr) -> crate::Span {
    match expr {
        Expr::String { span, .. }
        | Expr::RawString { span, .. }
        | Expr::Json { span, .. }
        | Expr::SchemaOf { span, .. }
        | Expr::Int { span, .. }
        | Expr::Float { span, .. }
        | Expr::Bool { span, .. }
        | Expr::List { span, .. }
        | Expr::Ref { span, .. }
        | Expr::Workflow { span }
        | Expr::Variant { span, .. }
        | Expr::Record { span, .. }
        | Expr::Field { span, .. }
        | Expr::Index { span, .. }
        | Expr::Accessor { span, .. }
        | Expr::Not { span, .. }
        | Expr::Binary { span, .. }
        | Expr::Predicate { span, .. }
        | Expr::CollectionPredicate { span, .. } => *span,
        Expr::Duration(duration) => duration.span,
    }
}
