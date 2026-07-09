//! Regression tests for four adversarial-review defects found in the
//! trailing-trivia, duplicate-field, and float-literal handling of AWL-0:
//!
//! 1. Trailing (same-line) comments were dropped on every field line except
//!    `as` — the printer silently ate `// ...` annotations on `when`, `do`,
//!    `timeout`, action routing fields, and handler `do`/terminal lines.
//! 2. An own-line comment after the final `finish` line (i.e. at the very
//!    end of the document, with no following line to attach to) was
//!    silently dropped.
//! 3. Duplicate occurrences of a single-valued field silently overwrote the
//!    first occurrence, so re-printing a document with an accidental
//!    duplicate field deleted the first occurrence's source text outright.
//! 4. Float literals were reformatted through `f64`, so `1.0` printed as `1`
//!    and reparsed as an `Int`, not a `Float`.

use std::error::Error;

use aion_awl::{CallTarget, Expr, StepOp, parse, print};

type TestResult = Result<(), Box<dyn Error>>;

// ---------------------------------------------------------------------
// 1. Trailing same-line comments on every field line
// ---------------------------------------------------------------------

const TRAILING_COMMENT_DOC: &str = "workflow trailing\noutput String\n\naction make() -> String\n  queue \"q1\"  // route note\n\nstep one\n  when true  // guard note\n  do make()  // call note\n  timeout 5s  // deadline note\n  on failure\n    do make()  // compensate note\n    fail\n  as out\n\nfinish out  // done note\n";

#[test]
fn trailing_comments_survive_on_every_field_kind() -> TestResult {
    let document = parse(TRAILING_COMMENT_DOC)?;
    let printed = print(&document);

    for needle in [
        "  queue \"q1\"  // route note",
        "  when true  // guard note",
        "  do make()  // call note",
        "  timeout 5s  // deadline note",
        "    do make()  // compensate note",
        "finish out  // done note",
    ] {
        assert!(
            printed.contains(needle),
            "missing trailing comment {needle:?} in:\n{printed}"
        );
    }
    Ok(())
}

#[test]
fn trailing_comment_document_round_trips_idempotently() -> TestResult {
    let first = print(&parse(TRAILING_COMMENT_DOC)?);
    let second = print(&parse(&first)?);
    assert_eq!(first, second);
    Ok(())
}

// ---------------------------------------------------------------------
// 2. Own-line comment after the final `finish` line (end of document)
// ---------------------------------------------------------------------

const EOF_COMMENT_DOC: &str =
    "workflow eof\noutput String\n\nfinish \"done\"\n\n// trailing epilogue comment\n";

#[test]
fn own_line_comment_after_finish_is_preserved() -> TestResult {
    let document = parse(EOF_COMMENT_DOC)?;
    assert_eq!(document.epilogue_comments.len(), 1);
    assert_eq!(
        document.epilogue_comments[0].text,
        "trailing epilogue comment"
    );

    let printed = print(&document);
    assert!(
        printed.contains("// trailing epilogue comment"),
        "epilogue comment missing from printed output:\n{printed}"
    );
    // It must come after the finish line, not before it.
    let finish_at = printed
        .find("finish \"done\"")
        .ok_or("finish line present in print")?;
    let comment_at = printed
        .find("// trailing epilogue comment")
        .ok_or("epilogue comment present in print")?;
    assert!(
        comment_at > finish_at,
        "epilogue comment printed before finish"
    );

    let reprinted = print(&parse(&printed)?);
    assert_eq!(reprinted, printed);
    Ok(())
}

// ---------------------------------------------------------------------
// 3. Duplicate single-valued fields are rejected, not last-write-wins
// ---------------------------------------------------------------------

/// Locate a uniquely-texted line (the common case: the second occurrence has
/// different literal text from the first, e.g. `when true` vs `when false`).
fn line_number(source: &str, needle: &str) -> Result<usize, Box<dyn Error>> {
    Ok(source
        .lines()
        .position(|line| line.trim() == needle)
        .map(|idx| idx + 1)
        .ok_or("needle must occur in source")?)
}

/// Locate the *second* line with identical literal text (for fields whose
/// duplicate occurrence reads exactly the same as the first, e.g. two `on
/// failure` headers).
fn second_occurrence_line(source: &str, needle: &str) -> Result<usize, Box<dyn Error>> {
    Ok(source
        .lines()
        .enumerate()
        .filter(|(_, line)| line.trim() == needle)
        .nth(1)
        .map(|(idx, _)| idx + 1)
        .ok_or("needle must occur at least twice")?)
}

#[test]
fn duplicate_when_is_rejected_at_the_second_occurrence() -> TestResult {
    let source = "workflow w\noutput String\n\naction make() -> String\n\nstep one\n  when true\n  when false\n  do make()\n\nfinish ok\n";
    let Err(error) = parse(source) else {
        return Err("second `when` field should be rejected".into());
    };
    assert!(
        error.message.contains("when"),
        "unexpected message: {}",
        error.message
    );
    assert_eq!(error.span.line, line_number(source, "when false")?);
    Ok(())
}

#[test]
fn duplicate_timeout_is_rejected_at_the_second_occurrence() -> TestResult {
    let source = "workflow w\noutput String\n\naction make() -> String\n\nstep one\n  do make()\n  timeout 5s\n  timeout 10s\n\nfinish ok\n";
    let Err(error) = parse(source) else {
        return Err("second `timeout` field should be rejected".into());
    };
    assert!(
        error.message.contains("timeout"),
        "unexpected message: {}",
        error.message
    );
    assert_eq!(error.span.line, line_number(source, "timeout 10s")?);
    Ok(())
}

#[test]
fn duplicate_as_is_rejected_at_the_second_occurrence() -> TestResult {
    let source = "workflow w\noutput String\n\naction make() -> String\n\nstep one\n  do make()\n  as a\n  as b\n\nfinish ok\n";
    let Err(error) = parse(source) else {
        return Err("second `as` field should be rejected".into());
    };
    assert!(
        error.message.contains("as"),
        "unexpected message: {}",
        error.message
    );
    assert_eq!(error.span.line, line_number(source, "as b")?);
    Ok(())
}

#[test]
fn duplicate_on_failure_block_is_rejected_at_the_second_occurrence() -> TestResult {
    let source = "workflow w\noutput String\n\naction make() -> String\n\nstep one\n  do make()\n  on failure\n    fail\n  on failure\n    fail\n\nfinish ok\n";
    let Err(error) = parse(source) else {
        return Err("second `on failure` block should be rejected".into());
    };
    assert!(
        error.message.contains("on failure"),
        "unexpected message: {}",
        error.message
    );
    assert_eq!(
        error.span.line,
        second_occurrence_line(source, "on failure")?
    );
    Ok(())
}

#[test]
fn duplicate_action_queue_is_rejected_at_the_second_occurrence() -> TestResult {
    let source = "workflow w\noutput String\n\naction make() -> String\n  queue \"q1\"\n  queue \"q2\"\n\nstep one\n  do make()\n\nfinish ok\n";
    let Err(error) = parse(source) else {
        return Err("second action `queue` field should be rejected".into());
    };
    assert!(
        error.message.contains("queue"),
        "unexpected message: {}",
        error.message
    );
    assert_eq!(error.span.line, line_number(source, "queue \"q2\"")?);
    Ok(())
}

// ---------------------------------------------------------------------
// 4. Float literals preserve their exact source lexeme
// ---------------------------------------------------------------------

#[test]
fn float_literals_round_trip_byte_identically() -> TestResult {
    let source = "workflow floats\noutput String\n\naction make(x: String) -> String\n\nstep one\n  do make(0.5)\n\nfinish 1.0\n";
    let document = parse(source)?;

    let Expr::Float {
        value: finish_value,
        ..
    } = &document.finish
    else {
        return Err("finish should stay a Float expression".into());
    };
    assert_eq!(finish_value, "1.0");

    let step = document.steps.first().ok_or("first step present")?;
    let StepOp::Do(CallTarget::Action(call)) = &step.op else {
        return Err("step op should be an action call".into());
    };
    let first_arg = call.args.first().ok_or("first call argument present")?;
    let Expr::Float {
        value: arg_value, ..
    } = first_arg
    else {
        return Err("call argument should stay a Float expression".into());
    };
    assert_eq!(arg_value, "0.5");

    let printed = print(&document);
    assert!(
        printed.contains("finish 1.0"),
        "expected literal `1.0` in:\n{printed}"
    );
    assert!(
        printed.contains("do make(0.5)"),
        "expected literal `0.5` in:\n{printed}"
    );

    let reprinted = print(&parse(&printed)?);
    assert_eq!(reprinted, printed);
    Ok(())
}

#[test]
fn float_with_trailing_zero_fraction_is_not_collapsed_to_an_int() -> TestResult {
    // `2.25` and `1.0` both exercise different digit shapes; neither may be
    // reformatted by round-tripping through `f64`.
    let source = "workflow floats2\noutput String\n\nfinish 2.25\n";
    let printed = print(&parse(source)?);
    assert!(printed.contains("finish 2.25"), "got:\n{printed}");

    let source_int_like = "workflow floats3\noutput String\n\nfinish 1.0\n";
    let printed_int_like = print(&parse(source_int_like)?);
    assert!(
        printed_int_like.contains("finish 1.0"),
        "`1.0` must not collapse to `1`, got:\n{printed_int_like}"
    );
    assert!(!printed_int_like.contains("finish 1\n"));
    Ok(())
}
