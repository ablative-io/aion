use crate::ast::{BinaryOp, DurationLiteral, Expr, RecordField, Spanned, join_span};
use crate::{DurationUnit, Keyword, Span, Token, TokenKind};

use super::ParseError;
use super::source::{SourceLine, lex_at};

pub(super) fn parse_expr_at(line: &SourceLine, text: &str) -> Result<Expr, ParseError> {
    let context = line.fragment_span(text);
    let tokens = lex_at(text, context)?;
    let mut parser = ExprParser {
        tokens: &tokens,
        pos: 0,
        context,
    };
    let expr = parser.parse_or()?;
    if parser.pos != tokens.len() {
        return Err(ParseError::new(
            tokens[parser.pos].span,
            "unexpected token in expression",
        ));
    }
    Ok(expr)
}

struct ExprParser<'a> {
    tokens: &'a [Token],
    pos: usize,
    context: Span,
}
impl ExprParser<'_> {
    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        self.parse_binary(Self::parse_and, &[Keyword::Or], &[BinaryOp::Or])
    }
    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        self.parse_binary(Self::parse_compare, &[Keyword::And], &[BinaryOp::And])
    }
    fn parse_compare(&mut self) -> Result<Expr, ParseError> {
        let left = self.parse_add()?;
        let Some(op) = self.comparison_op() else {
            return Ok(left);
        };
        let op_span = self
            .bump()
            .ok_or_else(|| ParseError::new(self.context, "expected comparison operator"))?
            .span;
        let right = self.parse_add()?;
        if self.comparison_op().is_some() {
            return Err(ParseError::new(op_span, "comparisons are non-associative"));
        }
        let span = join_span(left.span(), right.span());
        Ok(Expr::Binary {
            span,
            left: Box::new(left),
            op,
            right: Box::new(right),
        })
    }
    fn parse_add(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_not()?;
        while self.eat(&TokenKind::Plus) {
            let right = self.parse_not()?;
            let span = join_span(expr.span(), right.span());
            expr = Expr::Binary {
                span,
                left: Box::new(expr),
                op: BinaryOp::Add,
                right: Box::new(right),
            };
        }
        Ok(expr)
    }
    fn parse_not(&mut self) -> Result<Expr, ParseError> {
        if self.eat_keyword(Keyword::Not) {
            let expr = self.parse_not()?;
            let span = expr.span();
            Ok(Expr::Not {
                span,
                expr: Box::new(expr),
            })
        } else {
            self.parse_postfix()
        }
    }
    fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_primary()?;
        while self.eat(&TokenKind::Dot) {
            let token = self
                .bump()
                .ok_or_else(|| ParseError::new(self.context, "expected field name after `.`"))?;
            let field = match &token.kind {
                TokenKind::Identifier(name) => name.clone(),
                _ => return Err(ParseError::new(token.span, "expected field name after `.`")),
            };
            let span = join_span(expr.span(), token.span);
            expr = Expr::Field {
                span,
                base: Box::new(expr),
                field,
            };
        }
        Ok(expr)
    }
    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        let token = self
            .bump()
            .ok_or_else(|| ParseError::new(self.context, "expected expression"))?;
        let token_span = token.span;
        match token.kind {
            TokenKind::String(value) => Ok(Expr::String {
                span: token_span,
                value,
            }),
            TokenKind::Integer(value) => Ok(Expr::Int {
                span: token_span,
                value,
            }),
            TokenKind::Float(value) => Ok(Expr::Float {
                span: token_span,
                value,
            }),
            TokenKind::Duration { magnitude, unit } => Ok(Expr::Duration(DurationLiteral {
                span: token_span,
                magnitude,
                unit,
            })),
            TokenKind::Keyword(Keyword::True) => Ok(Expr::Bool {
                span: token_span,
                value: true,
            }),
            TokenKind::Keyword(Keyword::False) => Ok(Expr::Bool {
                span: token_span,
                value: false,
            }),
            TokenKind::Identifier(name) => {
                if self.peek_kind(&TokenKind::LeftParen) {
                    return Err(ParseError::new(
                        token_span,
                        "call expressions are only allowed as do targets",
                    ));
                }
                Ok(Expr::Ref {
                    span: token_span,
                    name,
                })
            }
            TokenKind::TypeIdentifier(name) if self.eat(&TokenKind::LeftParen) => {
                self.parse_record(token_span, name)
            }
            TokenKind::LeftBracket => self.parse_list(token_span),
            TokenKind::LeftParen => {
                let expr = self.parse_or()?;
                self.expect(&TokenKind::RightParen, "expected `)` after expression")?;
                Ok(expr)
            }
            _ => Err(ParseError::new(token_span, "expected expression")),
        }
    }
    fn parse_list(&mut self, start: Span) -> Result<Expr, ParseError> {
        let mut items = Vec::new();
        if self.eat(&TokenKind::RightBracket) {
            return Ok(Expr::List { span: start, items });
        }
        loop {
            items.push(self.parse_or()?);
            if self.eat(&TokenKind::RightBracket) {
                break;
            }
            self.expect(&TokenKind::Comma, "expected `,` between list items")?;
        }
        let end = items.last().map_or(start, Spanned::span);
        Ok(Expr::List {
            span: join_span(start, end),
            items,
        })
    }
    fn parse_record(&mut self, start: Span, name: String) -> Result<Expr, ParseError> {
        let mut fields = Vec::new();
        if self.eat(&TokenKind::RightParen) {
            return Ok(Expr::Record {
                span: start,
                name,
                fields,
            });
        }
        loop {
            let token = self
                .bump()
                .ok_or_else(|| ParseError::new(start, "unterminated record construction"))?;
            let token_span = token.span;
            let TokenKind::Identifier(field) = token.kind else {
                return Err(ParseError::new(token_span, "record field needs a name"));
            };
            self.expect(&TokenKind::Colon, "record field needs `:`")?;
            let value = self.parse_or()?;
            let field_span = join_span(token_span, value.span());
            fields.push(RecordField {
                span: field_span,
                name: field,
                value,
            });
            if self.eat(&TokenKind::RightParen) {
                break;
            }
            self.expect(&TokenKind::Comma, "unterminated record construction")?;
        }
        let end = fields.last().map_or(start, |field| field.span);
        Ok(Expr::Record {
            span: join_span(start, end),
            name,
            fields,
        })
    }
    fn parse_binary(
        &mut self,
        sub: fn(&mut Self) -> Result<Expr, ParseError>,
        kws: &[Keyword],
        ops: &[BinaryOp],
    ) -> Result<Expr, ParseError> {
        let mut expr = sub(self)?;
        loop {
            let Some(idx) = kws.iter().position(|kw| self.peek_keyword(*kw)) else {
                break;
            };
            self.bump();
            let right = sub(self)?;
            let span = join_span(expr.span(), right.span());
            expr = Expr::Binary {
                span,
                left: Box::new(expr),
                op: ops[idx],
                right: Box::new(right),
            };
        }
        Ok(expr)
    }
    fn comparison_op(&self) -> Option<BinaryOp> {
        self.tokens
            .get(self.pos)
            .and_then(|token| match &token.kind {
                TokenKind::EqualEqual => Some(BinaryOp::Eq),
                TokenKind::BangEqual => Some(BinaryOp::Ne),
                TokenKind::Less => Some(BinaryOp::Lt),
                TokenKind::LessEqual => Some(BinaryOp::Le),
                TokenKind::Greater => Some(BinaryOp::Gt),
                TokenKind::GreaterEqual => Some(BinaryOp::Ge),
                _ => None,
            })
    }
    fn peek_keyword(&self, kw: Keyword) -> bool {
        self.tokens
            .get(self.pos)
            .is_some_and(|t| t.kind == TokenKind::Keyword(kw))
    }
    fn eat_keyword(&mut self, kw: Keyword) -> bool {
        if self.peek_keyword(kw) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn peek_kind(&self, kind: &TokenKind) -> bool {
        self.tokens.get(self.pos).is_some_and(|t| &t.kind == kind)
    }
    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.peek_kind(kind) {
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
    fn bump(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.pos).cloned();
        self.pos += usize::from(token.is_some());
        token
    }
}

fn unit_text(unit: DurationUnit) -> &'static str {
    match unit {
        DurationUnit::Seconds => "s",
        DurationUnit::Minutes => "m",
        DurationUnit::Hours => "h",
        DurationUnit::Days => "d",
    }
}

pub(crate) fn duration_text(duration: &DurationLiteral) -> String {
    format!("{}{}", duration.magnitude, unit_text(duration.unit))
}
