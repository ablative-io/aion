//! Integration tests for AWL typechecking diagnostics.

use aion_awl::{CheckError, Span, check, parse};

fn errors(source: &str) -> Vec<CheckError> {
    match parse(source) {
        Ok(document) => check(&document),
        Err(error) => unreachable!("parse failed: {error}"),
    }
}

fn span_of(source: &str, needle: &str) -> Span {
    let Some(start) = source.find(needle) else {
        unreachable!("missing needle {needle:?}");
    };
    let prefix = &source[..start];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    Span {
        start,
        end: start + needle.len(),
        line,
        column: start - line_start + 1,
    }
}

fn assert_error(source: &str, expected_message: &str, needle: &str) {
    let errs = errors(source);
    let span = span_of(source, needle);
    assert!(
        errs.iter()
            .any(|error| error.message == expected_message && error.span.start == span.start),
        "missing {expected_message:?} at {span:?}; got {errs:#?}"
    );
    let Some(error) = errs
        .iter()
        .find(|error| error.message == expected_message && error.span.start == span.start)
    else {
        unreachable!();
    };
    assert_eq!(error.span.line, span.line);
    assert_eq!(error.span.column, span.column);
    assert_eq!(error.span.start, span.start);
}

#[test]
fn research_report_checks_without_errors() {
    let source = include_str!("fixtures/research_report.awl");
    assert_eq!(errors(source), Vec::<CheckError>::new());
}

#[test]
fn action_contract_errors_are_spanned_and_specific() {
    assert_error(
        "workflow w\noutput String\n\nstep s\n  do missing()\n\nfinish \"x\"\n",
        "unknown action `missing`",
        "missing()",
    );
    assert_error(
        "workflow w\noutput String\n\naction make(a: String) -> String\n\nstep s\n  do make()\n\nfinish \"x\"\n",
        "action `make` expected 1 argument(s), found 0",
        "make()",
    );
    assert_error(
        "workflow w\ninput a: Int\noutput String\n\naction make(a: String) -> String\n\nstep s\n  do make(a)\n\nfinish \"x\"\n",
        "argument `a` for action `make` expected String, found Int",
        "a)\n",
    );
}

#[test]
fn reference_resolution_and_rebinding_are_checked() {
    assert_error(
        "workflow w\noutput String\n\naction make() -> String\n\nstep s\n  do make()\n  as out\n\nfinish missing\n",
        "unresolved reference `missing`",
        "missing",
    );

    let positive = "workflow w\noutput String\n\naction make() -> String\naction make2(x: String) -> String\n\nstep s\n  do make()\n  as out\n\nstep t\n  do make2(out)\n  as out\n\nfinish out\n";
    assert_eq!(errors(positive), Vec::<CheckError>::new());

    assert_error(
        "workflow w\noutput String\n\naction make() -> String\naction count() -> Int\n\nstep s\n  do make()\n  as out\n\nstep t\n  do count()\n  as out\n\nfinish out\n",
        "as binding `out` expected String, found Int",
        "as out\n\nfinish",
    );
}

#[test]
fn expression_operator_errors_are_spanned_and_specific() {
    assert_error(
        "workflow w\ninput thing: Thing\noutput String\n\ntype Thing { ok: String }\n\nfinish thing.nope\n",
        "type `Thing` has no field `nope`",
        "thing.nope",
    );
    assert_error(
        "workflow w\ninput a: String\noutput String\n\nfinish a.nope\n",
        "field access expected record type, found String",
        "a.nope",
    );
    assert_error(
        "workflow w\ninput a: Int\noutput String\n\nfinish not a\n",
        "not operand expected Bool, found Int",
        "a\n",
    );
    assert_error(
        "workflow w\ninput a: Bool\ninput b: String\noutput String\n\nfinish a and b\n",
        "right boolean operand expected Bool, found String",
        "b",
    );
    assert_error(
        "workflow w\ninput a: Int\ninput b: String\noutput String\n\nfinish a == b\n",
        "comparison expected matching primitive operands, found Int and String",
        "a == b",
    );
    assert_error(
        "workflow w\ninput a: String\ninput b: Int\noutput String\n\nfinish a + b\n",
        "right + operand expected String, found Int",
        "b",
    );
    assert_error(
        "workflow w\noutput List(String)\n\nfinish [\"a\", 1]\n",
        "list element expected String, found Int",
        "1",
    );
}

#[test]
fn child_results_are_opaque_but_child_arguments_are_checked() {
    assert_error(
        "workflow w\ninput a: String\noutput String\n\nstep s\n  do child other(a)\n  as child_result\n\nfinish child_result.value\n",
        "child result is untyped in this revision and cannot be field-accessed",
        "child_result.value",
    );
    assert_error(
        "workflow w\noutput String\n\nstep s\n  do child other(missing)\n  as child_result\n\nfinish \"x\"\n",
        "unresolved reference `missing`",
        "missing",
    );
}

#[test]
fn record_construction_errors_are_spanned_and_specific() {
    assert_error(
        "workflow w\noutput String\n\nfinish Missing(a: 1)\n",
        "unknown record type `Missing`",
        "Missing(a: 1)",
    );
    assert_error(
        "workflow w\noutput Pair\n\ntype Pair { left: String, right: String }\n\nfinish Pair(left: \"x\")\n",
        "missing field `right` for record `Pair`",
        "Pair(left: \"x\")",
    );
    assert_error(
        "workflow w\noutput Pair\n\ntype Pair { left: String }\n\nfinish Pair(left: \"x\", right: \"y\")\n",
        "extra field `right` for record `Pair`",
        "right",
    );
    assert_error(
        "workflow w\noutput Pair\n\ntype Pair { left: String }\n\nfinish Pair(left: 1)\n",
        "field `left` expected String, found Int",
        "1",
    );
    assert_error(
        "workflow w\noutput Pair\n\ntype Pair { left: String }\n\nfinish Pair(left: \"x\", left: \"y\")\n",
        "duplicate field `left`",
        "left: \"y\"",
    );
}

#[test]
fn step_field_typing_is_covered_positively_and_negatively() {
    let positive = "workflow w\ninput flags: List(Bool)\noutput String\n\nsignal done: String\naction echo(flag: Bool, value: String) -> String\n\nstep wait_done\n  wait done\n  as value\n\nstep echo_each\n  when true\n  each flag in flags\n  do echo(flag, value)\n  repeat up to 2\n  until false\n  as values\n\nfinish value\n";
    assert_eq!(errors(positive), Vec::<CheckError>::new());

    assert_error(
        "workflow w\ninput a: String\noutput String\n\naction make() -> String\n\nstep s\n  when a\n  do make()\n\nfinish \"x\"\n",
        "when guard expected Bool, found String",
        "a\n  do",
    );
    assert_error(
        "workflow w\ninput a: String\noutput String\n\naction make() -> String\n\nstep s\n  each x in a\n  do make()\n\nfinish \"x\"\n",
        "each expression expected List(T), found String",
        "a\n  do",
    );
    assert_error(
        "workflow w\noutput String\n\nstep s\n  wait missing\n\nfinish \"x\"\n",
        "unknown signal `missing`",
        "wait missing",
    );
    assert_error(
        "workflow w\ninput a: String\noutput String\n\naction make() -> String\n\nstep s\n  do make()\n  until a\n\nfinish \"x\"\n",
        "until guard expected Bool, found String",
        "a\n\nfinish",
    );
    assert_error(
        "workflow w\ninput a: String\noutput String\n\naction make() -> String\n\nstep s\n  do make()\n  repeat up to a\n\nfinish \"x\"\n",
        "repeat up to expression expected Int, found String",
        "a\n\nfinish",
    );
    assert_error(
        "workflow w\noutput String\n\nstep s\n  sleep 1s\n  on timeout\n    finish 1\n\nfinish \"x\"\n",
        "handler finish expression expected String, found Int",
        "1\n\nfinish",
    );
    assert_error(
        "workflow w\noutput String\n\nfinish 1\n",
        "finish expression expected String, found Int",
        "1\n",
    );
}

#[test]
fn declaration_hygiene_and_identifier_rules_are_spanned() {
    assert_error(
        "workflow w\ninput a: String\ninput a: String\noutput String\n\nfinish a\n",
        "duplicate input declaration `a`",
        "input a: String\noutput",
    );
    assert_error(
        "workflow w\noutput String\n\ntype Thing { a: String }\ntype Thing { a: String }\n\nfinish \"x\"\n",
        "duplicate type declaration `Thing`",
        "type Thing { a: String }\n\nfinish",
    );
    assert_error(
        "workflow w\noutput String\n\nsignal s: String\nsignal s: String\n\nfinish \"x\"\n",
        "duplicate signal declaration `s`",
        "signal s: String\n\nfinish",
    );
    assert_error(
        "workflow w\noutput String\n\naction a() -> String\naction a() -> String\n\nfinish \"x\"\n",
        "duplicate action declaration `a`",
        "action a() -> String\n\nfinish",
    );
    assert_error(
        "workflow w\ninput badName: String\noutput String\n\nfinish \"x\"\n",
        "input name `badName` must be snake_case ([a-z][a-z0-9_]*)",
        "input badName: String",
    );
    assert_error(
        "workflow w\noutput String\n\ntype thing { a: String }\n\nfinish \"x\"\n",
        "type name `thing` must be TitleCase ([A-Z][A-Za-z0-9]*)",
        "type thing { a: String }",
    );
}
