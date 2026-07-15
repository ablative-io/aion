//! Type declarations: the three schema doors (shorthand records, inline raw
//! schema, file import), payload-less enums, and type references.

use crate::ast::{
    Comment, DocLine, EnumVariant, FieldDecl, Lead, TypeBody, TypeDecl, TypeRef, join_span,
};
use crate::{Keyword, Span, TokenKind};

use super::ParseError;
use super::hints::gone_type_hint;
use super::stream::{Stream, describe};

/// Parse a `type` declaration; the `type` keyword has been consumed and its
/// span is `keyword_span`.
pub(super) fn parse_type_decl(
    stream: &mut Stream,
    lead: Vec<Lead>,
    docs: Vec<DocLine>,
    keyword_span: Span,
) -> Result<TypeDecl, ParseError> {
    let (name, name_span) = stream.expect_name("a type name")?;
    let (body, trailing) = match stream.peek() {
        Some(token) if matches!(token.kind, TokenKind::LeftBrace) => {
            let open = token.span;
            stream.next();
            parse_record_body(stream, open)?
        }
        Some(token) if matches!(token.kind, TokenKind::Equal) => {
            stream.next();
            let body = parse_equals_body(stream)?;
            let trailing = stream.end_line()?;
            (body, trailing)
        }
        Some(token) => {
            return Err(ParseError::new(
                token.span,
                format!(
                    "expected `{{` or `=` after the type name, found {}",
                    describe(&token.kind)
                ),
            ));
        }
        None => {
            return Err(ParseError::new(
                stream.eof_span(),
                "expected `{` or `=` after the type name, found end of input",
            ));
        }
    };
    Ok(TypeDecl {
        span: join_span(keyword_span, name_span),
        lead,
        docs,
        trailing,
        name,
        name_span,
        body,
    })
}

/// Parse the body after `type Name =`: an enum, an inline raw schema, or a
/// file import.
fn parse_equals_body(stream: &mut Stream) -> Result<TypeBody, ParseError> {
    if let Some(schema) = stream.eat(|kind| matches!(kind, TokenKind::Keyword(Keyword::Schema))) {
        return parse_schema_door(stream, schema.span);
    }
    parse_enum_body(stream)
}

fn parse_schema_door(stream: &mut Stream, schema_span: Span) -> Result<TypeBody, ParseError> {
    match stream.peek() {
        Some(token) => {
            let span = token.span;
            match &token.kind {
                TokenKind::SchemaBody(body) => {
                    let body = body.clone();
                    validate_inline_schema(&body, span)?;
                    stream.next();
                    Ok(TypeBody::SchemaInline {
                        body,
                        body_span: span,
                    })
                }
                TokenKind::LeftParen => {
                    stream.next();
                    let (path, path_span) = match stream.peek() {
                        Some(token) => match &token.kind {
                            TokenKind::String(path) => (path.clone(), token.span),
                            other => {
                                return Err(ParseError::new(
                                    token.span,
                                    format!(
                                        "expected a schema file path string, found {}",
                                        describe(other)
                                    ),
                                ));
                            }
                        },
                        None => {
                            return Err(ParseError::new(
                                stream.eof_span(),
                                "expected a schema file path string, found end of input",
                            ));
                        }
                    };
                    stream.next();
                    stream.expect(
                        &TokenKind::RightParen,
                        "expected `)` after the schema file path",
                    )?;
                    Ok(TypeBody::SchemaImport { path, path_span })
                }
                other => Err(ParseError::new(
                    span,
                    format!(
                        "expected an inline body `schema {{ … }}` on this line or a file \
                         import `schema(\"path\")`, found {}",
                        describe(other)
                    ),
                )),
            }
        }
        None => Err(ParseError::new(
            schema_span,
            "expected `schema { … }` or `schema(\"path\")`, found end of input",
        )),
    }
}

/// Validate that an inline schema body is well-formed JSON, quoting the
/// offending lexeme with a document-true span when it is not. Schema
/// SEMANTICS (unsupported keywords, null types) are the checker's job; the
/// parser owns only JSON well-formedness.
fn validate_inline_schema(body: &str, body_span: Span) -> Result<(), ParseError> {
    let Err(error) = serde_json::from_str::<serde_json::Value>(body) else {
        return Ok(());
    };
    let (span, detail) = crate::jsontext::json_error_anchor(body, body_span, &error);
    Err(ParseError::new(
        span,
        format!("inline schema body is not valid JSON: {detail}"),
    ))
}

fn parse_enum_body(stream: &mut Stream) -> Result<TypeBody, ParseError> {
    let mut variants = Vec::new();
    loop {
        let (name, span) = stream.expect_name("an enum variant name")?;
        if let Some(open) = stream.eat(|kind| matches!(kind, TokenKind::LeftParen)) {
            return Err(ParseError::new(
                open.span,
                format!(
                    "payload-carrying enum variants are deferred: `{name}(…)` is not \
                     writable; variants are bare names"
                ),
            ));
        }
        variants.push(EnumVariant { span, name });
        if stream.eat(|kind| matches!(kind, TokenKind::Bar)).is_none() {
            return Ok(TypeBody::Enum { variants });
        }
    }
}

/// Parse a shorthand record body after its `{` has been consumed. Returns
/// the body and the trailing comment of the declaration's last line.
fn parse_record_body(
    stream: &mut Stream,
    open: Span,
) -> Result<(TypeBody, Option<Comment>), ParseError> {
    // Multi-line form: `{` ends the line and fields follow in a block.
    if stream.peek_is(|kind| matches!(kind, TokenKind::Newline)) {
        stream.next();
        if !stream.open_block() {
            return Err(ParseError::new(
                open,
                "expected an indented field block after `{`".to_owned(),
            ));
        }
        let mut fields = Vec::new();
        loop {
            let lead = stream.take_leads()?;
            if stream.at_item_block_end() {
                stream.push_back_leads(lead);
                break;
            }
            let docs = stream.take_docs();
            parse_field_line(stream, lead, docs, &mut fields)?;
        }
        let stray = stream.take_leads()?;
        stream.consume_block_dedent();
        stream.push_back_leads(stray);
        stream.expect(
            &TokenKind::RightBrace,
            "expected `}` to close the type body",
        )?;
        let trailing = stream.end_line()?;
        return Ok((TypeBody::Record { fields }, trailing));
    }
    // Single-line form: fields inline until `}`.
    let mut fields = Vec::new();
    loop {
        if stream
            .eat(|kind| matches!(kind, TokenKind::RightBrace))
            .is_some()
        {
            break;
        }
        let field = parse_field(stream, Vec::new(), Vec::new())?;
        fields.push(field);
        if stream
            .eat(|kind| matches!(kind, TokenKind::Comma))
            .is_some()
        {
            continue;
        }
        stream.expect(
            &TokenKind::RightBrace,
            "expected `}` to close the type body",
        )?;
        break;
    }
    let trailing = stream.end_line()?;
    Ok((TypeBody::Record { fields }, trailing))
}

/// Parse one physical line of a multi-line record body: one or more fields
/// (comma-tolerant both ways), then the line end.
fn parse_field_line(
    stream: &mut Stream,
    lead: Vec<Lead>,
    docs: Vec<DocLine>,
    fields: &mut Vec<FieldDecl>,
) -> Result<(), ParseError> {
    let mut field = parse_field(stream, lead, docs)?;
    loop {
        let had_comma = stream
            .eat(|kind| matches!(kind, TokenKind::Comma))
            .is_some();
        if stream.peek_is(|kind| {
            matches!(
                kind,
                TokenKind::Newline | TokenKind::Comment(_) | TokenKind::Dedent
            )
        }) || stream.peek().is_none()
        {
            field.trailing = stream.end_line()?;
            fields.push(field);
            return Ok(());
        }
        if !had_comma {
            let span = stream.peek_span();
            return Err(ParseError::new(span, "expected `,` between fields"));
        }
        fields.push(field);
        field = parse_field(stream, Vec::new(), Vec::new())?;
    }
}

fn parse_field(
    stream: &mut Stream,
    lead: Vec<Lead>,
    docs: Vec<DocLine>,
) -> Result<FieldDecl, ParseError> {
    let (name, name_span) = stream.expect_name("a field name")?;
    stream.expect(
        &TokenKind::Colon,
        "expected `:` between the field name and its type",
    )?;
    let ty = parse_type_ref(stream)?;
    Ok(FieldDecl {
        span: join_span(name_span, type_ref_span(&ty)),
        lead,
        docs,
        trailing: None,
        name,
        name_span,
        ty,
    })
}

/// Parse a type reference: `Name`, `[T]`, or a postfix-`?` optional.
pub(super) fn parse_type_ref(stream: &mut Stream) -> Result<TypeRef, ParseError> {
    let base = match stream.peek() {
        Some(token) => {
            let span = token.span;
            match &token.kind {
                TokenKind::TypeIdentifier(name) => {
                    let name = name.clone();
                    stream.next();
                    if stream.peek_is(|kind| matches!(kind, TokenKind::LeftParen)) {
                        let message = gone_type_hint(&name)
                            .unwrap_or_else(|| format!("type `{name}` takes no arguments"));
                        return Err(ParseError::new(span, message));
                    }
                    TypeRef::Named { span, name }
                }
                TokenKind::LeftBracket => {
                    stream.next();
                    let inner = parse_type_ref(stream)?;
                    let close = stream.expect(
                        &TokenKind::RightBracket,
                        "expected `]` to close the list type",
                    )?;
                    TypeRef::List {
                        span: join_span(span, close.span),
                        inner: Box::new(inner),
                    }
                }
                other => {
                    return Err(ParseError::new(
                        span,
                        format!("expected a type, found {}", describe(other)),
                    ));
                }
            }
        }
        None => {
            return Err(ParseError::new(
                stream.eof_span(),
                "expected a type, found end of input",
            ));
        }
    };
    if let Some(question) = stream.eat(|kind| matches!(kind, TokenKind::Question)) {
        return Ok(TypeRef::Optional {
            span: join_span(type_ref_span(&base), question.span),
            inner: Box::new(base),
        });
    }
    Ok(base)
}

/// The source span of a type reference.
pub(super) fn type_ref_span(ty: &TypeRef) -> Span {
    match ty {
        TypeRef::Named { span, .. }
        | TypeRef::List { span, .. }
        | TypeRef::Optional { span, .. } => *span,
    }
}
