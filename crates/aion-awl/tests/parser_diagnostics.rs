//! Parser-phase fix-round diagnostics: grammar-invalid shapes that were
//! previously accepted silently (duplicate loop tails, statements after
//! outcome clauses) and common author mistakes that deserve targeted
//! fix-its (a call as a pipe head). Every rejection is anchored on a
//! source-correct span.

use std::error::Error;

use aion_awl::parse;

type TestResult = Result<(), Box<dyn Error>>;

fn doc_with_body_line(line: &str) -> String {
    format!(
        "//! Parser diagnostics probe.\n\
         workflow probe\n\
         \x20 input a: String\n\
         \x20 outcome done: type Out, route success\n\
         \n\
         type Out {{ text: String }}\n\
         \n\
         worker w\n\
         \x20 action make(a: String) -> Out\n\
         \n\
         step one\n\
         \x20 {line}\n"
    )
}

fn doc_with_loop_tail_lines(tails: &str) -> String {
    doc_with_body_line(&format!(
        "loop x = Out(text: \"\")\n\
         \x20   make(a: a) -> x\n\
         {tails}"
    ))
}

// ---------------------------------------------------------------------
// Duplicate loop tails are rejected at the second keyword
// ---------------------------------------------------------------------

#[test]
fn duplicate_loop_until_is_rejected_at_the_second_keyword() -> TestResult {
    let source = doc_with_loop_tail_lines("    until true\n    until false");
    let Err(error) = parse(&source) else {
        return Err("a second `until` line should be rejected".into());
    };
    assert!(
        error.message.contains("`until` once"),
        "diagnostic must name the duplicate: {:?}",
        error.message
    );
    let anchor = source.find("until false").ok_or("second until present")?;
    assert_eq!(error.span.start, anchor, "{error:?}");
    let expected_line = source[..anchor].matches('\n').count() + 1;
    assert_eq!(error.span.line, expected_line, "{error:?}");
    Ok(())
}

#[test]
fn duplicate_loop_max_is_rejected_at_the_second_keyword() -> TestResult {
    let source = doc_with_loop_tail_lines("    until true\n    max 3\n    max 5");
    let Err(error) = parse(&source) else {
        return Err("a second `max` line should be rejected".into());
    };
    assert!(
        error.message.contains("`max` once"),
        "diagnostic must name the duplicate: {:?}",
        error.message
    );
    let anchor = source.find("max 5").ok_or("second max present")?;
    assert_eq!(error.span.start, anchor, "{error:?}");
    let expected_line = source[..anchor].matches('\n').count() + 1;
    assert_eq!(error.span.line, expected_line, "{error:?}");
    Ok(())
}

// ---------------------------------------------------------------------
// Outcome clauses close the step
// ---------------------------------------------------------------------

#[test]
fn statement_after_an_outcome_clause_is_rejected() -> TestResult {
    let source = doc_with_body_line(
        "make(a: a) -> out\n\
         \x20 outcome ok: when true, route done\n\
         \x20 make(a: out.text)",
    );
    let Err(error) = parse(&source) else {
        return Err("a statement below an outcome clause should be rejected".into());
    };
    assert!(
        error.message.contains("outcome clauses close the step"),
        "diagnostic must explain the ordering rule: {:?}",
        error.message
    );
    let anchor = source
        .find("make(a: out.text)")
        .ok_or("trailing statement present")?;
    let expected_line = source[..anchor].matches('\n').count() + 1;
    assert_eq!(error.span.line, expected_line, "{error:?}");
    Ok(())
}

// ---------------------------------------------------------------------
// A call is not a pipe head — targeted fix-it
// ---------------------------------------------------------------------

#[test]
fn call_headed_pipe_chain_gets_a_targeted_fix_it() -> TestResult {
    let source = doc_with_body_line("make(a: a) |> route done");
    let Err(error) = parse(&source) else {
        return Err("a call-headed pipe chain should be rejected".into());
    };
    assert!(
        error.message.contains("not a pipe head"),
        "diagnostic must explain the shape: {:?}",
        error.message
    );
    assert!(
        error.message.contains("->"),
        "diagnostic must show the bind-first fix: {:?}",
        error.message
    );
    let anchor = source.find("|> route done").ok_or("pipe present")?;
    assert_eq!(error.span.start, anchor, "{error:?}");
    Ok(())
}
