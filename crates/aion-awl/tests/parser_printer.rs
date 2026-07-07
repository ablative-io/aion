//! Integration tests for AWL parsing, canonical printing, and diagnostics.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::error::Error;

use aion_awl::{StepOp, parse, print};

fn assert_idempotent(source: &str) -> Result<(), Box<dyn Error>> {
    let first = print(&parse(source)?);
    let second = print(&parse(&first)?);
    assert_eq!(second, first);
    Ok(())
}

fn debug_without_spans<T: std::fmt::Debug>(value: &T) -> String {
    let debug = format!("{value:#?}");
    let mut out = String::new();
    let mut rest = debug.as_str();
    while let Some(start) = rest.find("span: Span {") {
        out.push_str(&rest[..start]);
        out.push_str("span: _");
        let after = &rest[start..];
        let Some(end) = after.find('}') else {
            break;
        };
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

#[test]
fn research_report_normalizes_to_golden_and_preserves_comments() -> Result<(), Box<dyn Error>> {
    let source = include_str!("fixtures/research_report.awl");
    let canonical = include_str!("fixtures/research_report.canonical.awl");
    let normalized_source = print(&parse(source)?);
    assert_eq!(
        debug_without_spans(&parse(&normalized_source)?),
        debug_without_spans(&parse(canonical)?)
    );
    let printed = normalized_source;
    assert_eq!(printed, canonical);
    assert_eq!(print(&parse(canonical)?), canonical);
    assert_eq!(print(&parse(&printed)?), printed);
    for comment in [
        "structured — parsed + validated from brief.json at start",
        "bulk — content-addressed snapshot handle (haematite)",
        "rebinds — every later step sees the revised draft",
    ] {
        assert!(canonical.contains(comment), "missing comment {comment}");
    }
    Ok(())
}

#[test]
fn bounded_cycle_normalizes_to_golden_and_is_idempotent() -> Result<(), Box<dyn Error>> {
    let source = include_str!("fixtures/bounded_cycle.awl");
    let canonical = include_str!("fixtures/bounded_cycle.canonical.awl");
    let normalized_source = print(&parse(source)?);
    assert_eq!(
        debug_without_spans(&parse(&normalized_source)?),
        debug_without_spans(&parse(canonical)?)
    );
    assert_eq!(print(&parse(source)?), canonical);
    assert_eq!(print(&parse(canonical)?), canonical);
    assert_idempotent(source)
}

#[test]
fn differently_formatted_same_document_print_identically() -> Result<(), Box<dyn Error>> {
    let fixture = include_str!("fixtures/research_report.awl");
    let variant = fixture
        .replace("input brief: Brief      //", "input brief: Brief //")
        .replace("\n\ninput corpus", "\n\n\ninput corpus")
        .replace("as draft                       //", "as draft //");
    assert_eq!(print(&parse(fixture)?), print(&parse(&variant)?));
    Ok(())
}

#[test]
fn mutating_ast_changes_printed_output() -> Result<(), Box<dyn Error>> {
    let mut document = parse(include_str!("fixtures/bounded_cycle.awl"))?;
    let step = document
        .steps
        .iter_mut()
        .find(|step| step.name == "fix")
        .unwrap();
    step.name = "repair".to_owned();
    if let StepOp::Do(_) = &step.op {
        let retry = step.retry.as_mut().unwrap();
        if let aion_awl::RetrySpec::Backoff { min, .. } = retry {
            min.magnitude = 10;
        }
    }
    let printed = print(&document);
    assert!(printed.contains("step repair"));
    assert!(printed.contains("retry 2 backoff 10s..1m"));
    Ok(())
}

#[test]
fn printer_is_idempotent_for_noncanonical_variants() -> Result<(), Box<dyn Error>> {
    let variants = [
        "workflow w\nabout x\n\ninput a: String\noutput String\n\naction make(a: String) -> String\n\nstep one\n  when true or false and not false\n  do make(a)\n  as out\n\nfinish out\n",
        "workflow w\ninput a: String   // c\noutput String\n\ntype Pair { left: String, right: String }\naction make(a: String) -> Pair\n\nstep one\n  do make(a)\n  as p\n\nfinish Pair(left: p.left, right: \"x\")\n",
        "workflow w\ninput a: String\noutput String\n\nsignal done: String\naction make(a: String) -> String\n\nstep one\n  wait done\n  timeout 1h\n  on timeout\n    finish \"timeout\"\n  as out\n\nfinish out\n",
    ];
    for variant in variants {
        assert_idempotent(variant)?;
    }
    Ok(())
}

#[test]
fn parse_errors_are_spanned_and_specific() {
    let cases = [
        (
            "workflow w\noutput String\n\nstep x\n  do a()\n  wait sig\n\nfinish ok\n",
            "exactly one",
        ),
        ("workflow w\noutput String\n", "missing finish"),
        (
            "workflow w\noutput String\n\nstep x\n  do a(b())\n\nfinish ok\n",
            "call expressions",
        ),
        (
            "workflow w\noutput String\n\nstep x\n  frob yes\n\nfinish ok\n",
            "unknown step field",
        ),
        (
            "workflow w\noutput String\n\nstep x\n  do a()\n\nfinish Out(a: 1\n",
            "unterminated record",
        ),
        (
            "workflow w\noutput String\n\nstep x\n  do a()\n  on failure\n  fail\n\nfinish ok\n",
            "wrong indentation",
        ),
    ];
    for (source, expected) in cases {
        let error = parse(source).expect_err(source);
        assert!(
            error.message.contains(expected),
            "{expected:?} not in {:?}",
            error.message
        );
        assert!(error.span.line > 0);
        assert!(error.span.column > 0);
    }
}
