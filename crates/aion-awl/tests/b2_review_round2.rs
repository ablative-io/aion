//! B2 review round 2: sibling-substep `after` edges join the scoped cycle
//! analysis (and refuse at lowering until honored), and binding-type
//! reconciliation is graph-join-aware — disjoint reuses of one name check
//! clean, incompatible types meeting at a join are rejected there, and
//! structurally equal joins keep a concrete type.

use std::error::Error;

use aion_awl::{CheckError, check, emit, parse};

type TestResult = Result<(), Box<dyn Error>>;

fn errors_of(source: &str) -> Result<Vec<CheckError>, Box<dyn Error>> {
    Ok(check(&parse(source)?))
}

fn line_col_of(source: &str, needle: &str) -> Result<(usize, usize), Box<dyn Error>> {
    let at = source
        .find(needle)
        .ok_or_else(|| format!("needle {needle:?} not in source"))?;
    let line = source[..at].matches('\n').count() + 1;
    let column = source[..at]
        .rsplit('\n')
        .next()
        .unwrap_or_default()
        .chars()
        .count()
        + 1;
    Ok((line, column))
}

fn assert_error_at(source: &str, substring: &str, needle: &str) -> TestResult {
    let errors = errors_of(source)?;
    let (line, column) = line_col_of(source, needle)?;
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
        return Err(format!("expected a clean check; got {rendered:#?}").into());
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Blocker — sibling-substep `after` dependencies
// ---------------------------------------------------------------------

/// The delta review's probe shape: `first after second` plus the
/// fall-through `first -> second` forms a mixed fall-through/`after` cycle,
/// caught by the sibling group's route-cycle analysis (the `after` edges
/// now feed it) with the re-arm diagnostic.
const SUBSTEP_AFTER_CYCLE: &str = "//! Substep after cycle.\n\
workflow substep_after_cycle\n\
\x20 input task: String\n\
\x20 outcome done: type String, route success\n\
\n\
worker w\n\
\x20 action stamp(text: String) -> String\n\
\n\
step run\n\
\x20 step first after second\n\
\x20\x20\x20 stamp(text: task) -> a\n\
\x20 step second\n\
\x20\x20\x20 stamp(text: a) -> b\n\
\x20 b |> route done\n";

#[test]
fn a_sibling_substep_after_cycle_is_rejected() -> TestResult {
    assert_error_at(
        SUBSTEP_AFTER_CYCLE,
        "re-arm each other (fall-through and `after` edges) in a cycle with no bound",
        "first after second",
    )
}

/// A PURE `after` cycle between siblings (both directions declared) is the
/// dependency-cycle defect, same diagnostic as the top level.
#[test]
fn a_pure_sibling_after_cycle_is_rejected() -> TestResult {
    let source = "//! Pure substep after cycle.\n\
workflow pure_substep_after_cycle\n\
\x20 input task: String\n\
\x20 outcome done: type String, route success\n\
\n\
worker w\n\
\x20 action stamp(text: String) -> String\n\
\n\
step run\n\
\x20 step first after second\n\
\x20\x20\x20 stamp(text: task) -> a\n\
\x20 step second after first\n\
\x20\x20\x20 stamp(text: a) -> b\n\
\x20 b |> route done\n";
    assert_error_at(
        source,
        "`after` dependencies form a cycle",
        "first after second",
    )
}

#[test]
fn an_unknown_sibling_after_target_is_rejected() -> TestResult {
    let source = "//! Unknown substep after.\n\
workflow unknown_substep_after\n\
\x20 input task: String\n\
\x20 outcome done: type String, route success\n\
\n\
worker w\n\
\x20 action stamp(text: String) -> String\n\
\n\
step run\n\
\x20 step first after does_not_exist\n\
\x20\x20\x20 stamp(text: task) -> a\n\
\x20 a |> route done\n";
    assert_error_at(
        source,
        "no step named `does_not_exist` exists among its sibling substeps",
        "does_not_exist\n",
    )
}

#[test]
fn substep_after_is_refused_at_emission() -> TestResult {
    let source = "//! Substep after refusal.\n\
workflow substep_after_refusal\n\
\x20 input task: String\n\
\x20 outcome done: type String, route success\n\
\n\
worker w\n\
\x20 action stamp(text: String) -> String\n\
\n\
step run\n\
\x20 step first\n\
\x20\x20\x20 stamp(text: task) -> a\n\
\x20 step second after first\n\
\x20\x20\x20 stamp(text: a) -> b\n\
\x20 b |> route done\n";
    let document = parse(source)?;
    assert!(
        check(&document).is_empty(),
        "the ordered form is legal at check"
    );
    let error = emit(&document).err().ok_or("emit must refuse")?;
    assert!(
        error.message.contains("not yet lowered")
            && error.message.contains("`after` dependencies on substeps"),
        "unexpected message: {}",
        error.message
    );
    let mir_error = aion_awl::mir::lower(&document, None)
        .err()
        .ok_or("mir lower must refuse")?;
    assert!(
        mir_error
            .to_string()
            .contains("`after` dependencies on substeps"),
        "unexpected mir message: {mir_error}"
    );
    Ok(())
}

// ---------------------------------------------------------------------
// Major — join-aware binding-type reconciliation
// ---------------------------------------------------------------------

/// The delta review's false-rejection probe: two mutually exclusive
/// terminal branches each bind a local `value` at different types and
/// never rejoin — legal under the ratified per-scope binding law.
const DISJOINT_REUSE: &str = "//! Disjoint reuse.\n\
workflow disjoint_reuse\n\
\x20 input essay: String\n\
\x20 outcome done_s: type String, route success\n\
\x20 outcome done_n: type Count, route failure\n\
\n\
type Flag  { which: Bool }\n\
type Count { n: Int }\n\
\n\
worker w\n\
\x20 action probe(essay: String) -> Flag\n\
\x20 action make_s(essay: String) -> String\n\
\x20 action make_n(essay: String) -> Count\n\
\n\
step decide\n\
\x20 probe(essay: essay) -> flag\n\
\n\
\x20 outcome go_s: when flag.which, route path_s\n\
\x20 outcome go_n: otherwise, route path_n\n\
\n\
step path_s\n\
\x20 make_s(essay: essay) -> value\n\
\x20 value |> route done_s\n\
\n\
step path_n\n\
\x20 make_n(essay: essay) -> value\n\
\x20 route done_n(n: value.n)\n";

#[test]
fn disjoint_branches_may_reuse_a_name_at_different_types() -> TestResult {
    assert_clean(DISJOINT_REUSE)
}

/// Two paths that DO rejoin with incompatible concrete types for one name
/// are rejected at the joining step.
const CONFLICTING_JOIN: &str = "//! Conflicting join.\n\
workflow conflicting_join\n\
\x20 input essay: String\n\
\x20 outcome done: type String, route success\n\
\n\
type Flag  { which: Bool }\n\
type Count { n: Int }\n\
\n\
worker w\n\
\x20 action probe(essay: String) -> Flag\n\
\x20 action make_s(essay: String) -> String\n\
\x20 action make_n(essay: String) -> Count\n\
\x20 action show(text: String) -> String\n\
\n\
step decide\n\
\x20 probe(essay: essay) -> flag\n\
\n\
\x20 outcome go_s: when flag.which, route path_s\n\
\x20 outcome go_n: otherwise, route path_n\n\
\n\
step path_s\n\
\x20 make_s(essay: essay) -> value\n\
\x20 route meet\n\
\n\
step path_n\n\
\x20 make_n(essay: essay) -> value\n\
\x20 route meet\n\
\n\
step meet\n\
\x20 show(text: value) -> out\n\
\x20 out |> route done\n";

#[test]
fn incompatible_types_meeting_at_a_join_are_rejected_there() -> TestResult {
    assert_error_at(
        CONFLICTING_JOIN,
        "paths that join must agree on a binding's type",
        "meet\n\x20 show",
    )
}

/// The delta review's type-loss probe: two paths bind structurally EQUAL
/// named records (`A` and `B` have the same shape), rejoin, and the joined
/// value keeps a CONCRETE type — compatible consumption passes and
/// incompatible consumption fails, instead of `Unknown` passing anything.
fn structural_join(consumer: &str) -> String {
    format!(
        "//! Structural join.\n\
workflow structural_join\n\
\x20 input essay: String\n\
\x20 outcome done: type String, route success\n\
\n\
type Flag {{ which: Bool }}\n\
type A    {{ text: String }}\n\
type B    {{ text: String }}\n\
\n\
worker w\n\
\x20 action probe(essay: String) -> Flag\n\
\x20 action make_a(essay: String) -> A\n\
\x20 action make_b(essay: String) -> B\n\
\x20 action eat_a(item: A) -> String\n\
\x20 action eat_s(text: String) -> String\n\
\n\
step decide\n\
\x20 probe(essay: essay) -> flag\n\
\n\
\x20 outcome go_a: when flag.which, route path_a\n\
\x20 outcome go_b: otherwise, route path_b\n\
\n\
step path_a\n\
\x20 make_a(essay: essay) -> value\n\
\x20 route meet\n\
\n\
step path_b\n\
\x20 make_b(essay: essay) -> value\n\
\x20 route meet\n\
\n\
step meet\n\
\x20 {consumer}\n\
\x20 out |> route done\n"
    )
}

#[test]
fn a_structurally_equal_join_keeps_a_concrete_type() -> TestResult {
    assert_clean(&structural_join("eat_a(item: value) -> out"))
}

#[test]
fn a_structurally_equal_join_rejects_incompatible_consumption() -> TestResult {
    let source = structural_join("eat_s(text: value) -> out");
    let errors = errors_of(&source)?;
    assert!(
        errors
            .iter()
            .any(|error| error.message.contains("expected") || error.message.contains("String")),
        "the joined record must not pass as a String; got no error"
    );
    Ok(())
}
