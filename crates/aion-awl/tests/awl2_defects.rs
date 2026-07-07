//! Regression tests for four adversarial-review defects in AWL-0:
//!
//! 1. Fragment lexing produced document-incorrect spans.
//! 2. Own-line comments before nested fields were dropped by the printer.
//! 3. Handler blocks (`on timeout` / `on failure`) accepted invalid shapes.
//! 4. Top-level declaration order was not enforced.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::format_push_string)]

use aion_awl::{Spanned, parse, print};

// ---------------------------------------------------------------------
// 1. Source-correct spans
// ---------------------------------------------------------------------

/// Build a document with a syntactically broken `when` expression on a
/// caller-chosen line, padding above it with valid filler steps so the
/// error sits deep in a multi-line document rather than on line 1.
fn doc_with_broken_expr_at_step(filler_steps: usize) -> String {
    let mut src = String::from("workflow padded\noutput String\n\naction noop() -> String\n\n");
    for i in 0..filler_steps {
        src.push_str(&format!("step filler{i}\n  do noop()\n\n"));
    }
    src.push_str("step bad\n  when 1 +\n  do noop()\n\nfinish \"done\"\n");
    src
}

#[test]
fn expression_error_deep_in_document_reports_true_line_and_offset() {
    // 12 filler steps push the broken expression well past line 1.
    let source = doc_with_broken_expr_at_step(12);
    let error = parse(&source).expect_err("`when 1 +` is not a valid expression");

    let expected_start = source.find("1 +").expect("fragment present in source");
    let expected_line = source[..expected_start].matches('\n').count() + 1;

    assert!(
        expected_line > 30,
        "test fixture didn't push the error deep enough: line {expected_line}"
    );
    assert_eq!(
        error.span.line, expected_line,
        "error must report the document line, not the fragment-relative line 1"
    );
    assert_eq!(
        error.span.start, expected_start,
        "error must report the true document byte offset"
    );
}

#[test]
fn expression_error_at_exactly_line_forty_is_source_correct() {
    // Tune the filler count until the broken line lands on line 40, then
    // assert against that literal line number end to end.
    let mut filler = 0;
    let source = loop {
        let candidate = doc_with_broken_expr_at_step(filler);
        let broken_line = candidate
            .lines()
            .position(|line| line.trim() == "when 1 +")
            .expect("broken line present")
            + 1;
        if broken_line == 40 {
            break candidate;
        }
        assert!(filler < 200, "could not land the broken line on line 40");
        filler += 1;
    };

    let error = parse(&source).expect_err("`when 1 +` is not a valid expression");
    assert_eq!(error.span.line, 40);
    let expected_start = source.find("1 +").unwrap();
    assert_eq!(error.span.start, expected_start);
}

#[test]
fn each_in_expr_span_matches_fixture_true_position() {
    // A real fixture (unmodified), not a synthetic string: prove an AST
    // node buried inside `parse()`'s output carries the document-true span,
    // not a span relative to the line fragment it was lexed from.
    let source = include_str!("fixtures/research_report.awl");
    let document = parse(source).expect("fixture parses");
    let investigate = document
        .steps
        .iter()
        .find(|step| step.name == "investigate")
        .expect("investigate step present");
    let each = investigate.each.as_ref().expect("each field present");

    // "each q in questions" appears once in the fixture, on line 31.
    let anchor = source.find("each q in questions").unwrap();
    let expected_start = anchor + "each q in ".len();

    assert_eq!(each.in_expr.span().line, 31);
    assert_eq!(each.in_expr.span().start, expected_start);
}

#[test]
fn duration_field_span_matches_fixture_true_position() {
    let source = include_str!("fixtures/research_report.awl");
    let document = parse(source).expect("fixture parses");
    let human_review = document
        .steps
        .iter()
        .find(|step| step.name == "human_review")
        .expect("human_review step present");
    let timeout = human_review.timeout.as_ref().expect("timeout present");

    // "timeout 3d" is on line 43.
    let anchor = source.find("timeout 3d").unwrap();
    let expected_start = anchor + "timeout ".len();
    assert_eq!(timeout.span.line, 43);
    assert_eq!(timeout.span.start, expected_start);
}

// ---------------------------------------------------------------------
// 2. Own-line comment trivia at any nesting level
// ---------------------------------------------------------------------

const COMMENT_TORTURE_DOC: &str = "workflow torture\nabout Comment placement torture test.\n\n// leading the first input\ninput a: String\n\n// leading output\noutput String\n\naction make(a: String) -> String\n  // leading queue\n  queue \"q1\"\n  // leading timeout\n  timeout 5s\n\nstep one\n  // leading when\n  when true\n  do make(a)\n  on failure\n    // leading do in failure handler\n    do make(a)\n    // leading fail terminal\n    fail\n  as out\n\n// leading finish\nfinish out\n";

#[test]
fn own_line_comments_survive_at_every_nesting_level() {
    let document = parse(COMMENT_TORTURE_DOC).expect("torture document parses");
    let printed = print(&document);

    let expectations = [
        ("// leading the first input", 0),
        ("// leading output", 0),
        ("  // leading queue", 2),
        ("  // leading timeout", 2),
        ("  // leading when", 2),
        ("    // leading do in failure handler", 4),
        ("    // leading fail terminal", 4),
        ("// leading finish", 0),
    ];
    for (needle, _indent) in expectations {
        assert!(
            printed.contains(needle),
            "missing or misindented comment {needle:?} in:\n{printed}"
        );
    }

    // Ordering: every comment must appear before the field it precedes, in
    // source order (not, say, all dumped together at the top or bottom).
    let mut previous = 0;
    for (needle, _) in expectations {
        let found = printed.find(needle).unwrap();
        assert!(found >= previous, "comment {needle:?} printed out of order");
        previous = found;
    }
}

#[test]
fn comment_torture_document_round_trips_idempotently() {
    let first = print(&parse(COMMENT_TORTURE_DOC).expect("parses"));
    let second = print(&parse(&first).expect("re-parses"));
    assert_eq!(first, second);
}

// ---------------------------------------------------------------------
// 3. Strict handler block shape
// ---------------------------------------------------------------------

fn doc_with_failure_body(body: &str) -> String {
    format!(
        "workflow w\noutput String\n\naction make() -> String\n\nstep one\n  do make()\n  on failure\n{body}  as out\n\nfinish out\n"
    )
}

#[test]
fn handler_block_rejects_a_second_terminal() {
    let source = doc_with_failure_body("    fail\n    fail\n");
    let error = parse(&source).expect_err("two terminals must be rejected");
    assert!(
        error.message.contains("exactly one terminal"),
        "unexpected message: {}",
        error.message
    );
    let expected_line = source
        .lines()
        .enumerate()
        .filter(|(_, line)| line.trim() == "fail")
        .nth(1)
        .map(|(idx, _)| idx + 1)
        .unwrap();
    assert_eq!(error.span.line, expected_line);
}

#[test]
fn handler_block_rejects_do_after_terminal() {
    let source = doc_with_failure_body("    fail\n    do make()\n");
    let error = parse(&source).expect_err("a do line after the terminal must be rejected");
    assert!(
        error.message.contains("must come before the terminal"),
        "unexpected message: {}",
        error.message
    );
    // Two lines read `do make()`: the step's own op field, and the handler's
    // `do` after `fail`. The error must point at the *later* one.
    let expected_line = source
        .lines()
        .enumerate()
        .filter(|(_, line)| line.trim() == "do make()")
        .last()
        .map(|(idx, _)| idx + 1)
        .unwrap();
    assert_eq!(error.span.line, expected_line);
}

#[test]
fn handler_block_rejects_finish_then_fail() {
    let source = doc_with_failure_body("    finish \"x\"\n    fail\n");
    let error = parse(&source).expect_err("a second terminal (fail after finish) must be rejected");
    assert!(
        error.message.contains("exactly one terminal"),
        "unexpected message: {}",
        error.message
    );
}

#[test]
fn handler_block_requires_a_terminal() {
    let source = doc_with_failure_body("    do make()\n");
    let error = parse(&source).expect_err("a handler block with no terminal must be rejected");
    assert!(
        error.message.contains("must finish or fail"),
        "unexpected message: {}",
        error.message
    );
    assert!(error.span.line > 0);
}

#[test]
fn well_formed_handler_block_still_parses_and_round_trips() {
    let source = doc_with_failure_body("    do make()\n    fail\n");
    let document = parse(&source).expect("well-formed handler block parses");
    let printed = print(&document);
    assert_eq!(print(&parse(&printed).unwrap()), printed);
}

// ---------------------------------------------------------------------
// 4. Canonical top-level declaration order
// ---------------------------------------------------------------------

#[test]
fn input_after_a_step_is_rejected_as_out_of_order() {
    let source =
        "workflow w\noutput String\n\nstep one\n  do make()\n\ninput a: String\n\nfinish ok\n";
    let error = parse(source).expect_err("input after step must be rejected");
    assert!(
        error.message.contains("out of canonical order"),
        "unexpected message: {}",
        error.message
    );
    let expected_line = source
        .lines()
        .position(|line| line.trim() == "input a: String")
        .map(|idx| idx + 1)
        .unwrap();
    assert_eq!(error.span.line, expected_line);
}

#[test]
fn type_after_action_is_rejected_as_out_of_order() {
    let source = "workflow w\noutput String\n\ntype T { a: String }\naction make() -> String\n\ntype U { b: String }\n\nstep one\n  do make()\n\nfinish ok\n";
    let error = parse(source).expect_err("type after action must be rejected");
    assert!(
        error.message.contains("out of canonical order"),
        "unexpected message: {}",
        error.message
    );
}

#[test]
fn duplicate_output_declaration_is_rejected() {
    let source = "workflow w\noutput String\n\noutput String\n\nfinish ok\n";
    let error = parse(source).expect_err("a second output declaration must be rejected");
    assert!(
        error.message.contains("duplicate `output`"),
        "unexpected message: {}",
        error.message
    );
}

#[test]
fn duplicate_error_declaration_is_rejected() {
    let source = "workflow w\noutput String\n\nerror Failed\n\nerror Failed\n\nfinish ok\n";
    let error = parse(source).expect_err("a second error declaration must be rejected");
    assert!(
        error.message.contains("duplicate `error`"),
        "unexpected message: {}",
        error.message
    );
}

#[test]
fn canonical_order_with_every_group_present_still_parses() {
    let source = "workflow w\nabout ok\n\ninput a: String\noutput String\nerror Failed\n\nsignal s: String\n\ntype Failed { reason: String }\n\naction make(a: String) -> String\n\nstep one\n  do make(a)\n\nfinish ok\n";
    parse(source).expect("fully-populated canonical-order document parses");
}
