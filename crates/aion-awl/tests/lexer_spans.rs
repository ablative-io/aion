//! Span-discipline tests for the AWL rev-2 lexer: tokens and lexical errors
//! on multi-line documents must report document-true byte offsets, lines, and
//! columns (the `awl2_defects` regression discipline, ported to rev-2 shapes).

use std::error::Error;
use std::io;

use aion_awl::{Keyword, LexError, TokenKind, lex};

type TestResult = Result<(), Box<dyn Error>>;

fn require_error(source: &str) -> Result<LexError, Box<dyn Error>> {
    match lex(source) {
        Ok(_) => Err(io::Error::other("expected a lexer error").into()),
        Err(error) => Ok(error),
    }
}

// ---------------------------------------------------------------------
// span discipline on multi-line documents
// ---------------------------------------------------------------------

/// Build a rev-2 document whose interesting line sits deep in the file,
/// padded above with a filler step of `filler_lines` single-line statements.
fn doc_with_line_deep(final_line: &str, filler_lines: usize) -> String {
    let mut src = String::from(
        "//! Span torture document.\nworkflow padded\n  input name: String\n  outcome done: type Out, route success\n\ntype Out { text: String }\n\nworker w\n  action noop(name: String) -> Out\n\nstep filler\n",
    );
    for _ in 0..filler_lines {
        src.push_str("  noop(name: name)\n");
    }
    src.push('\n');
    src.push_str(final_line);
    src.push('\n');
    src
}

#[test]
fn token_deep_in_document_reports_true_line_and_offset() -> TestResult {
    // Tune the filler count until the target line lands on line 40, then
    // assert the literal line number end to end.
    let target = "step deep\n  name |> noop |> route done";
    let mut filler = 0;
    let source = loop {
        let candidate = doc_with_line_deep(target, filler);
        let route_line = candidate
            .lines()
            .position(|line| line.trim() == "name |> noop |> route done")
            .ok_or("target line present")?
            + 1;
        if route_line == 40 {
            break candidate;
        }
        assert!(filler < 200, "could not land the target line on line 40");
        filler += 1;
    };

    let tokens = lex(&source)?;
    // The header's `route success` also carries a Route keyword; the target
    // is the one followed by the `done` identifier.
    let route = tokens
        .windows(2)
        .find(|pair| {
            pair[0].kind == TokenKind::Keyword(Keyword::Route)
                && pair[1].kind == TokenKind::Identifier("done".to_owned())
        })
        .map(|pair| &pair[0])
        .ok_or_else(|| io::Error::other("missing route-done token pair"))?;
    let expected_start = source.find("route done").ok_or("route present")?;
    assert_eq!(route.span.line, 40);
    assert_eq!(route.span.start, expected_start);
    assert_eq!(
        route.span.column,
        expected_start - source[..expected_start].rfind('\n').ok_or("newline")?
    );
    Ok(())
}

#[test]
fn lex_error_deep_in_document_reports_true_line_and_offset() -> TestResult {
    let source = doc_with_line_deep("step bad\n  greet(name: \"oops\\q\")", 25);
    let error = require_error(&source)?;

    let expected_start = source.find("\\q").ok_or("escape present in source")?;
    let expected_line = source[..expected_start].matches('\n').count() + 1;
    assert!(
        expected_line > 30,
        "fixture didn't push the error deep enough: line {expected_line}"
    );
    assert_eq!(error.span.line, expected_line);
    assert_eq!(error.span.start, expected_start);
    assert!(error.message.contains("escape"));
    Ok(())
}

#[test]
fn spans_stay_byte_correct_after_multibyte_doc_lines() -> TestResult {
    // The em dash is 3 bytes in UTF-8; the keyword after the doc line must
    // report byte offsets, not char counts.
    let source = "//! Nothing is pushed — the operator merges.\nworkflow handoff\n";
    let tokens = lex(source)?;
    let workflow = tokens
        .iter()
        .find(|token| token.kind == TokenKind::Keyword(Keyword::Workflow))
        .ok_or_else(|| io::Error::other("missing workflow token"))?;
    let expected_start = source.find("workflow").ok_or("workflow present")?;
    assert_eq!(workflow.span.start, expected_start);
    assert_eq!(workflow.span.line, 2);
    assert_eq!(workflow.span.column, 1);
    Ok(())
}

#[test]
fn tokens_expose_byte_line_and_column_spans() -> TestResult {
    let tokens = lex("workflow release_notes\n  wait approval -> decision\n")?;
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

// ---------------------------------------------------------------------
// diagnostics with spans
// ---------------------------------------------------------------------

#[test]
fn tab_in_indentation_reports_span() -> TestResult {
    let error = require_error("workflow x\n\tstep bad\n")?;
    assert_eq!(error.span.start, "workflow x\n".len());
    assert_eq!(error.span.end, "workflow x\n".len() + 1);
    assert_eq!(error.span.line, 2);
    assert_eq!(error.span.column, 1);
    assert!(error.message.contains("tabs"));
    Ok(())
}

#[test]
fn unterminated_string_reports_span() -> TestResult {
    let error = require_error("greet(name: \"no\n")?;
    assert_eq!(error.span.start, "greet(name: ".len());
    assert_eq!(error.span.line, 1);
    assert_eq!(error.span.column, 13);
    assert!(error.message.contains("unterminated"));
    Ok(())
}

#[test]
fn bad_escape_reports_escape_span() -> TestResult {
    let error = require_error("node \"bad\\r\"\n")?;
    assert_eq!(error.span.start, "node \"bad".len());
    assert_eq!(error.span.end, "node \"bad\\r".len());
    assert_eq!(error.span.line, 1);
    assert_eq!(error.span.column, 10);
    assert!(error.message.contains("escape"));
    Ok(())
}

#[test]
fn stray_character_reports_span() -> TestResult {
    let error = require_error("route @\n")?;
    assert_eq!(error.span.start, "route ".len());
    assert_eq!(error.span.end, "route @".len());
    assert_eq!(error.span.line, 1);
    assert_eq!(error.span.column, 7);
    assert!(error.message.contains("stray"));
    Ok(())
}

#[test]
fn lone_dot_without_field_name_reports_span() -> TestResult {
    let error = require_error("verdicts |> filter(. blocking)\n")?;
    let dot = "verdicts |> filter(".len();
    assert_eq!(error.span.start, dot);
    assert_eq!(error.span.line, 1);
    assert_eq!(error.span.column, dot + 1);
    assert!(error.message.contains("field name"));
    Ok(())
}

#[test]
fn crlf_newline_spans_cover_the_full_terminator() -> TestResult {
    // On tolerated CRLF input the Newline token spans `\r\n`, not just the
    // stripped content boundary (which would point at the `\r` byte alone).
    let source = "workflow x\r\nstep s\r\n";
    let tokens = lex(source)?;
    let newlines: Vec<_> = tokens
        .iter()
        .filter(|token| token.kind == TokenKind::Newline)
        .collect();
    assert_eq!(newlines.len(), 2);
    assert_eq!(newlines[0].span.start, "workflow x".len());
    assert_eq!(newlines[0].span.end, "workflow x\r\n".len());
    assert_eq!(newlines[0].span.line, 1);
    assert_eq!(newlines[0].span.column, "workflow x".len() + 1);
    assert_eq!(newlines[1].span.start, "workflow x\r\nstep s".len());
    assert_eq!(newlines[1].span.end, "workflow x\r\nstep s\r\n".len());
    assert_eq!(newlines[1].span.line, 2);
    Ok(())
}

#[test]
fn lf_newline_span_is_the_single_terminator_byte() -> TestResult {
    let tokens = lex("workflow x\n")?;
    let newline = tokens
        .iter()
        .find(|token| token.kind == TokenKind::Newline)
        .ok_or_else(|| io::Error::other("missing newline token"))?;
    assert_eq!(newline.span.start, "workflow x".len());
    assert_eq!(newline.span.end, "workflow x\n".len());
    Ok(())
}

#[test]
fn columns_after_multibyte_content_on_the_same_line_are_char_based() -> TestResult {
    // `é` is two bytes; the stray `@` is the 20th character of the line, so
    // the diagnostic must say column 20 (byte offsets stay byte-true).
    let source = "note(text: \"café\") @\n";
    let error = require_error(source)?;
    assert_eq!(error.span.start, source.find('@').ok_or("@ present")?);
    assert_eq!(error.span.line, 1);
    assert_eq!(error.span.column, 20);

    let tokens = lex("greet(name: \"café\") -> greeting\n")?;
    let arrow = tokens
        .iter()
        .find(|token| token.kind == TokenKind::Arrow)
        .ok_or_else(|| io::Error::other("missing arrow token"))?;
    assert_eq!(arrow.span.start, "greet(name: \"café\") ".len());
    // 20 characters precede the arrow (`é` counts once), so column 21.
    assert_eq!(arrow.span.column, 21);
    Ok(())
}

#[test]
fn over_indentation_jump_reports_a_lexical_error() -> TestResult {
    // No rev-2 construct opens two indentation levels at once; synthesizing
    // phantom Indent tokens would hand the parser structure the source does
    // not contain.
    let error = require_error("step one\n    provision(a: b)\n")?;
    assert_eq!(error.span.start, "step one\n".len());
    assert_eq!(error.span.end, "step one\n    ".len());
    assert_eq!(error.span.line, 2);
    assert_eq!(error.span.column, 1);
    assert!(
        error.message.contains("more than one"),
        "unexpected message: {}",
        error.message
    );
    Ok(())
}

#[test]
fn odd_indentation_reports_span() -> TestResult {
    let error = require_error("step one\n  sleep 30s\n   sleep 5m\n")?;
    assert_eq!(error.span.line, 3);
    assert!(
        error.message.contains("two-space"),
        "unexpected message: {}",
        error.message
    );
    Ok(())
}
