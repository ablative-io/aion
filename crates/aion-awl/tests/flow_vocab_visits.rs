//! Flow-vocabulary B2 visits rules: the `max … visits` cycle rule (decoy
//! loops rejected, input-derived bounds enforced), the `visits` builtin's
//! guard-only scope, and the sanctioned cycle-threaded rebinding — every
//! diagnostic asserted at its line and column.

use std::error::Error;

use aion_awl::{CheckError, check, parse};

type TestResult = Result<(), Box<dyn Error>>;

fn errors_of(source: &str) -> Result<Vec<CheckError>, Box<dyn Error>> {
    Ok(check(&parse(source)?))
}

/// Line and column (1-based) of the first occurrence of `needle`.
fn line_col_of(source: &str, needle: &str) -> Result<(usize, usize), Box<dyn Error>> {
    let start = source
        .find(needle)
        .ok_or_else(|| format!("needle {needle:?} not found"))?;
    Ok(line_col_at(source, start))
}

/// Line and column (1-based) of the LAST occurrence of `needle`.
fn line_col_of_last(source: &str, needle: &str) -> Result<(usize, usize), Box<dyn Error>> {
    let start = source
        .rfind(needle)
        .ok_or_else(|| format!("needle {needle:?} not found"))?;
    Ok(line_col_at(source, start))
}

fn line_col_at(source: &str, start: usize) -> (usize, usize) {
    let prefix = &source[..start];
    let line = prefix.matches('\n').count() + 1;
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    (line, start - line_start + 1)
}

/// Assert a diagnostic contains `substring`, anchored at the first
/// occurrence of `needle` in the source.
fn assert_error_at(source: &str, substring: &str, needle: &str) -> TestResult {
    let (line, column) = line_col_of(source, needle)?;
    assert_error_at_position(source, substring, line, column)
}

/// Assert a diagnostic contains `substring`, anchored at the LAST
/// occurrence of `needle` in the source.
fn assert_error_at_last(source: &str, substring: &str, needle: &str) -> TestResult {
    let (line, column) = line_col_of_last(source, needle)?;
    assert_error_at_position(source, substring, line, column)
}

fn assert_error_at_position(
    source: &str,
    substring: &str,
    line: usize,
    column: usize,
) -> TestResult {
    let errors = errors_of(source)?;
    let matched = errors.iter().any(|error| {
        error.message.contains(substring) && error.span.line == line && error.span.column == column
    });
    if !matched {
        let rendered: Vec<String> = errors
            .iter()
            .map(|error| {
                format!(
                    "{}:{}: {}",
                    error.span.line, error.span.column, error.message
                )
            })
            .collect();
        return Err(format!(
            "no diagnostic containing {substring:?} at {line}:{column}; got {rendered:#?}"
        )
        .into());
    }
    Ok(())
}

fn assert_clean(source: &str) -> TestResult {
    let errors = errors_of(source)?;
    if !errors.is_empty() {
        let rendered: Vec<String> = errors
            .iter()
            .map(|error| {
                format!(
                    "{}:{}: {}",
                    error.span.line, error.span.column, error.message
                )
            })
            .collect();
        return Err(format!("expected a clean check, got {rendered:#?}").into());
    }
    Ok(())
}

/// The shared scaffold: one input list, one outcome, one worker.
fn doc(steps: &str) -> String {
    format!(
        "//! Region rules.\n\
         workflow t\n\
         \x20 input items: [String]\n\
         \x20 outcome done: type String, route success\n\
         \n\
         worker w\n\
         \x20 action work(item: String) -> String\n\
         \x20 action expand(item: String) -> Batch\n\
         \n\
         type Batch {{ parts: [String] }}\n\
         \n\
         {steps}"
    )
}

// ---------------------------------------------------------------------
// The visits cycle rule and the `visits` builtin
// ---------------------------------------------------------------------

#[test]
fn a_cycle_bounded_only_by_an_inner_loop_is_now_rejected() -> TestResult {
    let source = doc("step compose\n\
         \x20 loop draft = \"\" counting rounds\n\
         \x20   work(item: draft) -> draft\n\
         \x20   until draft == \"ready\"\n\
         \x20   max 3\n\
         \n\
         \x20 outcome ready: when draft == \"ready\", route review\n\
         \x20 outcome stuck: otherwise, route done(draft)\n\
         \n\
         step review\n\
         \x20 work(item: draft) -> verdict\n\
         \n\
         \x20 outcome approved: when verdict == \"ok\", route done(verdict)\n\
         \x20 outcome rework: otherwise, route compose\n");
    assert_error_at_last(&source, "not a cycle bound", "compose\n")
}

#[test]
fn a_visits_bound_on_a_cycle_member_makes_the_cycle_legal() -> TestResult {
    let source = doc("step compose\n\
         \x20 work(item: \"seed\") -> draft\n\
         \n\
         step review\n\
         \x20 work(item: draft) -> verdict\n\
         \n\
         \x20 outcome approved: when verdict == \"ok\", route done(verdict)\n\
         \x20 outcome rework: otherwise, route compose\n\
         \x20 max 4 visits\n");
    assert_clean(&source)
}

#[test]
fn the_visits_bound_must_be_input_derived() -> TestResult {
    let source = doc("step compose\n\
         \x20 work(item: \"seed\") -> draft\n\
         \x20 work(item: draft) -> ceiling\n\
         \n\
         step review\n\
         \x20 work(item: draft) -> verdict\n\
         \n\
         \x20 outcome approved: when verdict == \"ok\", route done(verdict)\n\
         \x20 outcome rework: otherwise, route compose\n\
         \x20 max ceiling visits\n");
    assert_error_at(&source, "fixed before the flow starts", "ceiling visits")
}

#[test]
fn the_visits_bound_must_be_an_int() -> TestResult {
    let source = "//! Bound type.\n\
                  workflow t\n\
                  \x20 input label: String\n\
                  \x20 outcome done: type String, route success\n\
                  \n\
                  worker w\n\
                  \x20 action work(item: String) -> String\n\
                  \n\
                  step compose\n\
                  \x20 work(item: \"seed\") -> draft\n\
                  \n\
                  step review\n\
                  \x20 work(item: draft) -> verdict\n\
                  \n\
                  \x20 outcome approved: when verdict == \"ok\", route done(verdict)\n\
                  \x20 outcome rework: otherwise, route compose\n\
                  \x20 max label visits\n";
    assert_error_at(source, "needs an Int bound", "label visits")
}

#[test]
fn visits_is_unreadable_outside_the_bounded_steps_guards() -> TestResult {
    // In a body statement of the bounded step:
    let source = doc("step compose\n\
         \x20 work(item: \"seed\") -> draft\n\
         \n\
         step review\n\
         \x20 work(item: draft) -> verdict\n\
         \x20 work(item: \"n\" + visits) -> echo\n\
         \n\
         \x20 outcome approved: when verdict == \"ok\", route done(verdict)\n\
         \x20 outcome rework: otherwise, route compose\n\
         \x20 max 4 visits\n");
    assert_error_at(&source, "builtin visit counter", "visits) -> echo")?;
    // And in the guards of a step with no bound:
    let source = doc("step alone\n\
         \x20 work(item: \"x\") -> note\n\
         \n\
         \x20 outcome retry: when visits < 2, route done(note)\n\
         \x20 outcome ok: otherwise, route done(note)\n");
    assert_error_at(&source, "builtin visit counter", "visits < 2")
}

// ---------------------------------------------------------------------
// Cycle-threaded rebinding (the §6 `fold` shape)
// ---------------------------------------------------------------------

#[test]
fn a_cycle_step_may_rebind_a_cycle_threaded_name_keeping_its_type() -> TestResult {
    let source = doc("step plan\n\
         \x20 work(item: \"seed\") -> state\n\
         \n\
         step fold after plan\n\
         \x20 work(item: state) -> state\n\
         \n\
         \x20 outcome next: when state == \"\", route fold\n\
         \x20 outcome finish: otherwise, route done(state)\n\
         \x20 max 3 visits\n");
    assert_clean(&source)
}

#[test]
fn a_cycle_rebinding_that_changes_the_type_is_rejected() -> TestResult {
    let source = doc("step plan\n\
         \x20 work(item: \"seed\") -> state\n\
         \n\
         step fold after plan\n\
         \x20 expand(item: state) -> state\n\
         \n\
         \x20 outcome next: when state.parts is empty, route fold\n\
         \x20 outcome finish: otherwise, route done(\"over\")\n\
         \x20 max 3 visits\n");
    assert_error_at(&source, "must keep its type", "state\n\n\x20 outcome next")
}

#[test]
fn a_non_cycle_step_still_may_not_rebind() -> TestResult {
    let source = doc("step plan\n\
         \x20 work(item: \"seed\") -> state\n\
         \n\
         step tail\n\
         \x20 work(item: state) -> state\n\
         \x20 state |> route done\n");
    assert_error_at(&source, "already bound", "state\n\x20 state |> route done")
}
