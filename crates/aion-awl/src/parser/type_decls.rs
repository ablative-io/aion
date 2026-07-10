use crate::ast::{FieldDecl, Trivia, TypeDecl, join_span};

use super::ParseError;
use super::calls::comma_parts;
use super::document::LineParser;
use super::source::{SourceLine, keyword_rest};
use super::types::parse_type_at;

pub(super) fn parse_type_decl(parser: &mut LineParser) -> Result<TypeDecl, ParseError> {
    let header = parser.bump_required("missing type declaration after peek")?;
    let attached = parser.take_description(&header, 0);
    let description = attached.as_ref().map(|doc| doc.text.clone());
    let description_source = attached.map(|doc| doc.source);
    let trivia = parser.take_trivia(&header);
    let rest = keyword_rest(&header, "type", "type declaration needs record fields")?;
    let (name, body) = rest
        .split_once('{')
        .ok_or_else(|| ParseError::new(header.span, "type declaration needs record fields"))?;

    if let Some(body) = body.strip_suffix('}') {
        return Ok(TypeDecl {
            span: header.span,
            trivia,
            description,
            description_source,
            name: name.trim().to_owned(),
            fields: parse_fields(&header, body, &Trivia::default(), None, None)?,
        });
    }
    if !body.trim().is_empty() {
        return Err(ParseError::new(
            header.span,
            "multi-line type header must end after `{`",
        ));
    }

    let mut fields = Vec::new();
    loop {
        let line = parser
            .peek()
            .ok_or_else(|| ParseError::new(header.span, "unterminated type record"))?;
        if line.indent == 0 && line.code == "}" {
            let closing = parser.bump_required("missing type record closing brace")?;
            let _ = parser.take_trivia(&closing);
            return Ok(TypeDecl {
                span: join_span(header.span, closing.span),
                trivia,
                description,
                description_source,
                name: name.trim().to_owned(),
                fields,
            });
        }
        if line.indent != 2 {
            return Err(ParseError::new(
                line.span,
                "type fields must be indented two spaces",
            ));
        }
        let field_line = parser.bump_required("missing type field after peek")?;
        let attached = parser.take_description(&field_line, 2);
        let field_description = attached.as_ref().map(|doc| doc.text.clone());
        let field_description_source = attached.map(|doc| doc.source);
        let field_trivia = parser.take_trivia(&field_line);
        fields.extend(parse_fields(
            &field_line,
            field_line
                .code
                .strip_suffix(',')
                .unwrap_or(&field_line.code),
            &field_trivia,
            field_description.as_deref(),
            field_description_source.as_deref(),
        )?);
    }
}

fn parse_fields(
    line: &SourceLine,
    body: &str,
    trivia: &Trivia,
    description: Option<&str>,
    description_source: Option<&str>,
) -> Result<Vec<FieldDecl>, ParseError> {
    let mut fields = Vec::new();
    for part in comma_parts(body) {
        if part.trim().is_empty() {
            continue;
        }
        let (field, ty) = part
            .split_once(':')
            .ok_or_else(|| ParseError::new(line.span, "type field needs `name: Type`"))?;
        fields.push(FieldDecl {
            span: line.span,
            trivia: trivia.clone(),
            description: description.map(str::to_owned),
            description_source: description_source.map(str::to_owned),
            name: field.trim().to_owned(),
            ty: parse_type_at(line, ty.trim())?,
        });
    }
    Ok(fields)
}
