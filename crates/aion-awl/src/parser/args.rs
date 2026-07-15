//! Named-argument lists and `->` bindings — the leaf grammar shared by
//! calls, record construction, route payloads, spawns, waits, and pipe
//! terminators.

use crate::TokenKind;
use crate::ast::{Arg, Binding, join_span};

use super::ParseError;
use super::exprs::{expr_span, parse_expr};
use super::stream::{Stream, describe};

/// Parse a named-argument list after its opening parenthesis has been
/// consumed. Returns the arguments and the closing parenthesis span.
///
/// Every argument must be named (`name: expr`); a positional value is
/// refused at the offending token.
pub(super) fn parse_args(stream: &mut Stream) -> Result<(Vec<Arg>, crate::Span), ParseError> {
    let mut args = Vec::new();
    loop {
        if let Some(close) = stream.eat(|kind| matches!(kind, TokenKind::RightParen)) {
            return Ok((args, close.span));
        }
        args.push(parse_arg(stream)?);
        if stream
            .eat(|kind| matches!(kind, TokenKind::Comma))
            .is_some()
        {
            continue;
        }
        let close = stream.expect(
            &TokenKind::RightParen,
            "expected `)` to close the argument list",
        )?;
        return Ok((args, close.span));
    }
}

fn parse_arg(stream: &mut Stream) -> Result<Arg, ParseError> {
    let (name, name_span) = match stream.peek() {
        Some(token) => match &token.kind {
            TokenKind::Identifier(name) => (name.clone(), token.span),
            TokenKind::Keyword(keyword) if super::stream::soft_keyword(*keyword) => {
                (keyword.as_word().to_owned(), token.span)
            }
            other => {
                return Err(ParseError::new(
                    token.span,
                    format!(
                        "arguments must be named (`name: value`), found {}",
                        describe(other)
                    ),
                ));
            }
        },
        None => {
            return Err(ParseError::new(
                stream.eof_span(),
                "arguments must be named (`name: value`); expected an argument name",
            ));
        }
    };
    stream.next();
    if stream
        .eat(|kind| matches!(kind, TokenKind::Colon))
        .is_none()
    {
        return Err(ParseError::new(
            name_span,
            format!("arguments must be named: write `{name}: <value>`"),
        ));
    }
    let value = parse_expr(stream)?;
    Ok(Arg {
        span: join_span(name_span, expr_span(&value)),
        name,
        name_span,
        value,
    })
}

/// Parse a `-> name` binding after the arrow has been consumed; `arrow` is
/// the arrow's span, used to anchor the missing-name diagnostic.
pub(super) fn parse_binding(
    stream: &mut Stream,
    arrow: crate::Span,
) -> Result<Binding, ParseError> {
    match stream.peek() {
        Some(token) => {
            let span = token.span;
            match &token.kind {
                TokenKind::Identifier(name) => {
                    let name = name.clone();
                    stream.next();
                    Ok(Binding { span, name })
                }
                TokenKind::Keyword(keyword) => Err(ParseError::new(
                    span,
                    format!(
                        "`->` must be followed by a binding name; `{}` is a reserved keyword",
                        keyword.as_word()
                    ),
                )),
                other => Err(ParseError::new(
                    if matches!(other, TokenKind::Newline) {
                        arrow
                    } else {
                        span
                    },
                    format!(
                        "expected a binding name after `->`, found {}",
                        describe(other)
                    ),
                )),
            }
        }
        None => Err(ParseError::new(
            arrow,
            "expected a binding name after `->`, found end of input",
        )),
    }
}
