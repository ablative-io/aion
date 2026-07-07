//! Integration tests for AWL lexer fixture handling, spans, and diagnostics.

use std::error::Error;
use std::io;

use aion_awl::{DurationUnit, Keyword, LexError, Token, TokenKind, lex};

fn token_kinds(tokens: &[Token]) -> Vec<TokenKind> {
    tokens.iter().map(|token| token.kind.clone()).collect()
}

fn require_error(source: &str) -> Result<LexError, Box<dyn Error>> {
    match lex(source) {
        Ok(_) => Err(io::Error::other("expected a lexer error").into()),
        Err(error) => Ok(error),
    }
}

#[test]
fn fixture_lexes_and_human_review_sequence_is_exact() -> Result<(), Box<dyn Error>> {
    let source = include_str!("fixtures/research_report.awl");
    let tokens = lex(source)?;
    let kinds = token_kinds(&tokens);

    let start = kinds
        .windows(2)
        .position(|window| {
            window
                == [
                    TokenKind::Keyword(Keyword::Step),
                    TokenKind::Identifier("human_review".to_owned()),
                ]
        })
        .ok_or_else(|| io::Error::other("missing human_review step"))?;
    let relative_end = kinds[start + 2..]
        .windows(2)
        .position(|window| {
            window
                == [
                    TokenKind::Keyword(Keyword::Step),
                    TokenKind::Identifier("revise".to_owned()),
                ]
        })
        .ok_or_else(|| io::Error::other("missing revise step after human_review"))?;
    let end = start + 2 + relative_end;

    let expected = vec![
        TokenKind::Keyword(Keyword::Step),
        TokenKind::Identifier("human_review".to_owned()),
        TokenKind::Newline,
        TokenKind::Indent,
        TokenKind::Keyword(Keyword::About),
        TokenKind::Prose(
            "Durable gate — parks free while idle, survives restarts, resumable weeks later."
                .to_owned(),
        ),
        TokenKind::Newline,
        TokenKind::Keyword(Keyword::Wait),
        TokenKind::Identifier("review".to_owned()),
        TokenKind::Newline,
        TokenKind::Keyword(Keyword::Timeout),
        TokenKind::Duration {
            magnitude: 3,
            unit: DurationUnit::Days,
        },
        TokenKind::Newline,
        TokenKind::Keyword(Keyword::On),
        TokenKind::Keyword(Keyword::Timeout),
        TokenKind::Newline,
        TokenKind::Indent,
        TokenKind::Keyword(Keyword::Finish),
        TokenKind::TypeIdentifier("Published".to_owned()),
        TokenKind::LeftParen,
        TokenKind::Identifier("report".to_owned()),
        TokenKind::Colon,
        TokenKind::Identifier("draft".to_owned()),
        TokenKind::Comma,
        TokenKind::Identifier("url".to_owned()),
        TokenKind::Colon,
        TokenKind::String(String::new()),
        TokenKind::RightParen,
        TokenKind::Newline,
        TokenKind::Dedent,
        TokenKind::Keyword(Keyword::As),
        TokenKind::Identifier("approval".to_owned()),
        TokenKind::Newline,
        TokenKind::Dedent,
    ];

    assert_eq!(&kinds[start..end], expected.as_slice());
    assert!(kinds.iter().any(|kind| matches!(kind, TokenKind::Indent)));
    assert!(kinds.iter().any(|kind| matches!(kind, TokenKind::Dedent)));
    Ok(())
}

#[test]
fn about_lines_capture_unquoted_prose_verbatim_to_line_end() -> Result<(), Box<dyn Error>> {
    let tokens = lex("about   // Keep comment markers and \"quotes\" as prose\n")?;
    let kinds = token_kinds(&tokens);

    assert_eq!(
        kinds,
        vec![
            TokenKind::Keyword(Keyword::About),
            TokenKind::Prose("// Keep comment markers and \"quotes\" as prose".to_owned()),
            TokenKind::Newline,
        ]
    );
    Ok(())
}

#[test]
fn duration_literals_require_immediate_unit_suffix() -> Result<(), Box<dyn Error>> {
    let tokens = lex("sleep 30s\nsleep 10m\nsleep 2h\nsleep 3d\nsleep 30 s\n")?;
    let kinds = token_kinds(&tokens);
    let durations: Vec<_> = kinds
        .iter()
        .filter_map(|kind| match kind {
            TokenKind::Duration { magnitude, unit } => Some((*magnitude, *unit)),
            _ => None,
        })
        .collect();

    assert_eq!(
        durations,
        vec![
            (30, DurationUnit::Seconds),
            (10, DurationUnit::Minutes),
            (2, DurationUnit::Hours),
            (3, DurationUnit::Days),
        ]
    );
    assert!(
        kinds.windows(2).any(|window| {
            window
                == [
                    TokenKind::Integer(30),
                    TokenKind::Identifier("s".to_owned()),
                ]
        }),
        "30 s must not lex as a duration"
    );
    Ok(())
}

#[test]
fn nested_blocks_emit_two_space_indents_and_dedents() -> Result<(), Box<dyn Error>> {
    let source = "step publish\n  on failure\n    do delete_assets(assets)\n    fail\n  as url\n";
    let kinds = token_kinds(&lex(source)?);

    assert_eq!(
        kinds,
        vec![
            TokenKind::Keyword(Keyword::Step),
            TokenKind::Identifier("publish".to_owned()),
            TokenKind::Newline,
            TokenKind::Indent,
            TokenKind::Keyword(Keyword::On),
            TokenKind::Keyword(Keyword::Failure),
            TokenKind::Newline,
            TokenKind::Indent,
            TokenKind::Keyword(Keyword::Do),
            TokenKind::Identifier("delete_assets".to_owned()),
            TokenKind::LeftParen,
            TokenKind::Identifier("assets".to_owned()),
            TokenKind::RightParen,
            TokenKind::Newline,
            TokenKind::Keyword(Keyword::Fail),
            TokenKind::Newline,
            TokenKind::Dedent,
            TokenKind::Keyword(Keyword::As),
            TokenKind::Identifier("url".to_owned()),
            TokenKind::Newline,
            TokenKind::Dedent,
        ]
    );
    Ok(())
}

#[test]
fn tab_in_indentation_reports_span() -> Result<(), Box<dyn Error>> {
    let error = require_error("workflow x\n\tstep bad\n")?;
    assert_eq!(error.span.start, "workflow x\n".len());
    assert_eq!(error.span.end, "workflow x\n".len() + 1);
    assert_eq!(error.span.line, 2);
    assert_eq!(error.span.column, 1);
    assert!(error.message.contains("tabs"));
    Ok(())
}

#[test]
fn tokens_expose_byte_line_and_column_spans() -> Result<(), Box<dyn Error>> {
    let tokens = lex("workflow research_report\n  wait review\n")?;
    let first = &tokens[0];
    assert_eq!(first.kind, TokenKind::Keyword(Keyword::Workflow));
    assert_eq!(first.span.start, 0);
    assert_eq!(first.span.end, "workflow".len());
    assert_eq!(first.span.line, 1);
    assert_eq!(first.span.column, 1);

    let wait = tokens
        .iter()
        .find(|token| token.kind == TokenKind::Keyword(Keyword::Wait))
        .ok_or_else(|| io::Error::other("missing wait token"))?;
    assert_eq!(wait.span.line, 2);
    assert_eq!(wait.span.column, 3);
    Ok(())
}

#[test]
fn unterminated_string_reports_span() -> Result<(), Box<dyn Error>> {
    let error = require_error("do call(\"no\n")?;
    assert_eq!(error.span.start, "do call(".len());
    assert_eq!(error.span.line, 1);
    assert_eq!(error.span.column, 9);
    assert!(error.message.contains("unterminated"));
    Ok(())
}

#[test]
fn bad_escape_reports_escape_span() -> Result<(), Box<dyn Error>> {
    let error = require_error("queue \"bad\\r\"\n")?;
    assert_eq!(error.span.start, "queue \"bad".len());
    assert_eq!(error.span.end, "queue \"bad\\r".len());
    assert_eq!(error.span.line, 1);
    assert_eq!(error.span.column, 11);
    assert!(error.message.contains("escape"));
    Ok(())
}

#[test]
fn stray_character_reports_span() -> Result<(), Box<dyn Error>> {
    let error = require_error("finish @\n")?;
    assert_eq!(error.span.start, "finish ".len());
    assert_eq!(error.span.end, "finish @".len());
    assert_eq!(error.span.line, 1);
    assert_eq!(error.span.column, 8);
    assert!(error.message.contains("stray"));
    Ok(())
}
