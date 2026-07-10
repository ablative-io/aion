//! Lexer tests for the inline `schema { … }` type door. The spec promises
//! "paste an existing JSON Schema verbatim", so the lexer raw-captures the
//! brace-balanced body into a single `SchemaBody` token — negative numbers,
//! exponent literals, JSON string escapes (`\uXXXX`, `\/`), and arbitrary
//! indentation all pass through byte-for-byte, exempt from AWL lexical rules.

use std::error::Error;
use std::io;

use aion_awl::{Keyword, Token, TokenKind, lex};

type TestResult = Result<(), Box<dyn Error>>;

fn token_kinds(tokens: &[Token]) -> Vec<TokenKind> {
    tokens.iter().map(|token| token.kind.clone()).collect()
}

fn kw(keyword: Keyword) -> TokenKind {
    TokenKind::Keyword(keyword)
}

fn type_ident(name: &str) -> TokenKind {
    TokenKind::TypeIdentifier(name.to_owned())
}

/// The exact source slice from the opening `{` through the matching final
/// `}`, which is what the `SchemaBody` payload must reproduce byte-for-byte.
fn braced_slice(source: &str) -> Result<&str, Box<dyn Error>> {
    let open = source.find('{').ok_or("source has an opening brace")?;
    let close = source.rfind('}').ok_or("source has a closing brace")?;
    Ok(&source[open..=close])
}

#[test]
fn inline_schema_body_is_one_verbatim_token() -> TestResult {
    let source = "type Round = schema {\n  \"type\": \"object\",\n  \"required\": [\"summary\", \"gates_green\"],\n  \"properties\": {\n    \"summary\":     { \"type\": \"string\" },\n    \"gates_green\": { \"type\": \"boolean\" }\n  }\n}\n";
    let kinds = token_kinds(&lex(source)?);
    assert_eq!(
        kinds,
        vec![
            kw(Keyword::Type),
            type_ident("Round"),
            TokenKind::Equal,
            kw(Keyword::Schema),
            TokenKind::SchemaBody(braced_slice(source)?.to_owned()),
            TokenKind::Newline,
        ]
    );
    Ok(())
}

#[test]
fn negative_numbers_exponents_and_json_escapes_pass_verbatim() -> TestResult {
    // Every item here is legal JSON Schema the AWL token grammar cannot
    // express: signed numbers, e-notation (both cases), `\uXXXX` and `\/`
    // string escapes.
    let source = "type X = schema {\n  \"type\": \"integer\",\n  \"minimum\": -1,\n  \"multipleOf\": 1e-3,\n  \"maximum\": 1E5,\n  \"description\": \"caf\\u00e9 a\\/b\"\n}\n";
    let tokens = lex(source)?;
    let body = tokens
        .iter()
        .find_map(|token| match &token.kind {
            TokenKind::SchemaBody(text) => Some(text.as_str()),
            _ => None,
        })
        .ok_or_else(|| io::Error::other("missing SchemaBody token"))?;
    assert_eq!(body, braced_slice(source)?);
    assert!(body.contains("\"minimum\": -1"));
    assert!(body.contains("1e-3"));
    assert!(body.contains("1E5"));
    assert!(body.contains("caf\\u00e9 a\\/b"));
    Ok(())
}

#[test]
fn pasted_schema_with_non_two_space_indentation_lexes() -> TestResult {
    // A schema pasted from elsewhere keeps its own indentation (three
    // spaces here); the raw region is exempt from the two-space rule.
    let source = "type X = schema {\n   \"type\": \"object\",\n   \"properties\": {\n      \"id\": { \"type\": \"string\" }\n   }\n}\n";
    let tokens = lex(source)?;
    let body = tokens
        .iter()
        .find_map(|token| match &token.kind {
            TokenKind::SchemaBody(text) => Some(text.as_str()),
            _ => None,
        })
        .ok_or_else(|| io::Error::other("missing SchemaBody token"))?;
    assert_eq!(body, braced_slice(source)?);
    Ok(())
}

#[test]
fn single_line_schema_body_keeps_lexing_the_rest_of_the_line() -> TestResult {
    let source = "type X = schema { \"type\": \"integer\", \"minimum\": -1 } // pasted\n";
    let kinds = token_kinds(&lex(source)?);
    assert_eq!(
        kinds,
        vec![
            kw(Keyword::Type),
            type_ident("X"),
            TokenKind::Equal,
            kw(Keyword::Schema),
            TokenKind::SchemaBody("{ \"type\": \"integer\", \"minimum\": -1 }".to_owned()),
            TokenKind::Comment("pasted".to_owned()),
            TokenKind::Newline,
        ]
    );
    Ok(())
}

#[test]
fn braces_and_escaped_quotes_inside_json_strings_do_not_close_the_body() -> TestResult {
    let source = "type X = schema {\n  \"pattern\": \"^{[0-9]+}$\",\n  \"description\": \"say \\\"hi\\\" {now}\"\n}\n";
    let tokens = lex(source)?;
    let body = tokens
        .iter()
        .find_map(|token| match &token.kind {
            TokenKind::SchemaBody(text) => Some(text.as_str()),
            _ => None,
        })
        .ok_or_else(|| io::Error::other("missing SchemaBody token"))?;
    assert_eq!(body, braced_slice(source)?);
    Ok(())
}

#[test]
fn no_structural_tokens_are_emitted_inside_a_multiline_body() -> TestResult {
    // The opening line emits no Newline of its own; exactly one Newline
    // follows the SchemaBody (for the closing-brace line), and no
    // Indent/Dedent tokens leak out of the raw region.
    let source = "type X = schema {\n  \"type\": \"object\"\n}\n";
    let kinds = token_kinds(&lex(source)?);
    let newlines = kinds
        .iter()
        .filter(|kind| **kind == TokenKind::Newline)
        .count();
    assert_eq!(newlines, 1);
    assert!(!kinds.contains(&TokenKind::Indent));
    assert!(!kinds.contains(&TokenKind::Dedent));
    assert_eq!(kinds.last(), Some(&TokenKind::Newline));
    Ok(())
}

#[test]
fn schema_body_span_is_document_true() -> TestResult {
    let source = "//! Doc.\nworkflow w\n  input x: X\n  outcome done: type X, route success\n\ntype X = schema {\n  \"type\": \"object\"\n}\n";
    let tokens = lex(source)?;
    let body = tokens
        .iter()
        .find(|token| matches!(token.kind, TokenKind::SchemaBody(_)))
        .ok_or_else(|| io::Error::other("missing SchemaBody token"))?;
    let open = source.find("schema {").ok_or("door present")? + "schema ".len();
    let close = source.rfind('}').ok_or("closing brace present")?;
    assert_eq!(body.span.start, open);
    assert_eq!(body.span.end, close + 1);
    assert_eq!(body.span.line, 6);
    assert_eq!(body.span.column, "type X = schema ".len() + 1);
    Ok(())
}

#[test]
fn tokens_after_a_multiline_body_report_the_closing_line() -> TestResult {
    let source =
        "type X = schema {\n  \"type\": \"object\"\n} // tail note\ntype Y { id: String }\n";
    let tokens = lex(source)?;
    let comment = tokens
        .iter()
        .find(|token| matches!(token.kind, TokenKind::Comment(_)))
        .ok_or_else(|| io::Error::other("missing trailing comment token"))?;
    assert_eq!(comment.kind, TokenKind::Comment("tail note".to_owned()));
    assert_eq!(comment.span.line, 3);
    assert_eq!(comment.span.column, 3);
    assert_eq!(comment.span.start, source.find("// tail").ok_or("comment")?);

    let second_type = tokens
        .iter()
        .find(|token| token.kind == TokenKind::TypeIdentifier("Y".to_owned()))
        .ok_or_else(|| io::Error::other("missing type Y token"))?;
    assert_eq!(second_type.span.line, 4);
    Ok(())
}

#[test]
fn unterminated_schema_body_reports_the_opening_brace() -> TestResult {
    let source = "type X = schema {\n  \"type\": \"object\"\n";
    let Err(error) = lex(source) else {
        return Err(io::Error::other("expected a lexer error").into());
    };
    let open = source.find('{').ok_or("opening brace present")?;
    assert_eq!(error.span.start, open);
    assert_eq!(error.span.end, open + 1);
    assert_eq!(error.span.line, 1);
    assert_eq!(error.span.column, open + 1);
    assert!(error.message.contains("unterminated"));
    Ok(())
}

#[test]
fn schema_file_import_door_is_not_raw_captured() -> TestResult {
    let kinds = token_kinds(&lex(
        "type Brief = schema(\"schemas/brief.schema.json\")\n",
    )?);
    assert_eq!(
        kinds,
        vec![
            kw(Keyword::Type),
            type_ident("Brief"),
            TokenKind::Equal,
            kw(Keyword::Schema),
            TokenKind::LeftParen,
            TokenKind::String("schemas/brief.schema.json".to_owned()),
            TokenKind::RightParen,
            TokenKind::Newline,
        ]
    );
    Ok(())
}

#[test]
fn brace_on_the_line_after_schema_is_not_a_raw_door() -> TestResult {
    // The door is `schema {` on one line; a brace on the next line lexes as
    // an ordinary LeftBrace for the parser to refuse with a targeted message.
    let kinds = token_kinds(&lex("type X = schema\n{\n")?);
    assert_eq!(
        kinds,
        vec![
            kw(Keyword::Type),
            type_ident("X"),
            TokenKind::Equal,
            kw(Keyword::Schema),
            TokenKind::Newline,
            TokenKind::LeftBrace,
            TokenKind::Newline,
        ]
    );
    Ok(())
}
