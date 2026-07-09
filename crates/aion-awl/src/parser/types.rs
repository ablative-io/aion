use crate::ast::TypeRef;
use crate::{Span, Token, TokenKind};

use super::ParseError;
use super::source::{SourceLine, lex_at};

pub(super) fn parse_type_at(line: &SourceLine, text: &str) -> Result<TypeRef, ParseError> {
    let context = line.fragment_span(text);
    let tokens = lex_at(text, context)?;
    let mut parser = TypeParser {
        tokens: &tokens,
        pos: 0,
        context,
    };
    let ty = parser.parse_type()?;
    if parser.pos != tokens.len() {
        return Err(ParseError::new(
            tokens[parser.pos].span,
            "unexpected token in type",
        ));
    }
    Ok(ty)
}

struct TypeParser<'a> {
    tokens: &'a [Token],
    pos: usize,
    context: Span,
}
impl TypeParser<'_> {
    fn parse_type(&mut self) -> Result<TypeRef, ParseError> {
        let token = self
            .bump()
            .ok_or_else(|| ParseError::new(self.context, "expected type"))?;
        let token_span = token.span;
        match token.kind {
            TokenKind::TypeIdentifier(name) => {
                if (name == "List" || name == "Option") && self.eat(&TokenKind::LeftParen) {
                    let inner = self.parse_type()?;
                    self.expect(&TokenKind::RightParen, "expected `)` after type parameter")?;
                    if name == "List" {
                        Ok(TypeRef::List {
                            span: token_span,
                            inner: Box::new(inner),
                        })
                    } else {
                        Ok(TypeRef::Option {
                            span: token_span,
                            inner: Box::new(inner),
                        })
                    }
                } else {
                    Ok(TypeRef::Named {
                        span: token_span,
                        name,
                    })
                }
            }
            _ => Err(ParseError::new(token_span, "expected type name")),
        }
    }
    fn bump(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.pos).cloned();
        self.pos += usize::from(token.is_some());
        token
    }
    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.tokens.get(self.pos).is_some_and(|t| &t.kind == kind) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, kind: &TokenKind, msg: &str) -> Result<(), ParseError> {
        if self.eat(kind) {
            Ok(())
        } else {
            Err(ParseError::new(
                self.tokens.get(self.pos).map_or(self.context, |t| t.span),
                msg,
            ))
        }
    }
}
