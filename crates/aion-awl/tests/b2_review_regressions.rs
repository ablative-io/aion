//! B2 review-round regressions, one section per finding: failure-route
//! provenance (routes out of `on failure` carry the step's ENTRY set, not
//! the successful body's bindings), sibling-substep cycle analysis, the
//! every-directed-cycle visits-bound rule, the one-type-per-binding-name
//! merge rule, const/subflow-parameter shadowing, and same-named subflow
//! outcome references.

use std::error::Error;

use aion_awl::semantic::analyze;
use aion_awl::{CheckError, check, emit, parse};

type TestResult = Result<(), Box<dyn Error>>;

fn errors_of(source: &str) -> Result<Vec<CheckError>, Box<dyn Error>> {
    Ok(check(&parse(source)?))
}

fn line_col_at(source: &str, start: usize) -> (usize, usize) {
    let prefix = &source[..start];
    let line = prefix.matches('\n').count() + 1;
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    (line, start - line_start + 1)
}

fn line_col_of(source: &str, needle: &str) -> Result<(usize, usize), Box<dyn Error>> {
    let start = source
        .find(needle)
        .ok_or_else(|| format!("needle {needle:?} not found"))?;
    Ok(line_col_at(source, start))
}

fn line_col_of_last(source: &str, needle: &str) -> Result<(usize, usize), Box<dyn Error>> {
    let start = source
        .rfind(needle)
        .ok_or_else(|| format!("needle {needle:?} not found"))?;
    Ok(line_col_at(source, start))
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

fn assert_error_at(source: &str, substring: &str, needle: &str) -> TestResult {
    let (line, column) = line_col_of(source, needle)?;
    assert_error_at_position(source, substring, line, column)
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

// ---------------------------------------------------------------------
// Blocker 1 — failure routes carry the entry set, not the body's bindings
// ---------------------------------------------------------------------

const FAILURE_BINDING_DOC: &str = "//! Failure-route provenance probe.\n\
workflow probe\n\
\x20 input task: String\n\
\x20 outcome done: type String, route success\n\
\n\
worker w\n\
\x20 action work(item: String) -> String\n\
\x20 action risky(item: String) -> String\n\
\n\
step produce\n\
\x20 risky(item: task) -> value\n\
\n\
\x20 on failure\n\
\x20   route consume\n\
\n\
\x20 outcome ok: otherwise, route consume\n\
\n\
step consume\n\
\x20 work(item: value) -> shown\n\
\x20 shown |> route done\n";

#[test]
fn a_failure_route_does_not_inherit_success_bindings() -> TestResult {
    // `value` is bound only when `risky` succeeds; the `on failure` route
    // reaches `consume` without it, so the read is not guaranteed.
    assert_error_at(
        FAILURE_BINDING_DOC,
        "not guaranteed on every path",
        "value) -> shown",
    )
}

#[test]
fn the_failure_binding_document_is_refused_before_emission() -> TestResult {
    // The emit-after-check invariant: the document must never reach the
    // Gleam backend (where the unbound name failed `gleam build`).
    let document = parse(FAILURE_BINDING_DOC)?;
    let error = emit(&document).err().ok_or("emit must refuse")?;
    assert!(
        error.message.contains("does not check cleanly"),
        "unexpected message: {}",
        error.message
    );
    Ok(())
}

#[test]
fn a_failure_route_into_a_collect_breaks_definite_assignment() -> TestResult {
    let source = "//! Failure route into a strict collect.\n\
workflow probe\n\
\x20 input items: [String]\n\
\x20 outcome done: type String, route success\n\
\n\
worker w\n\
\x20 action work(item: String) -> String\n\
\x20 action risky(item: String) -> String\n\
\n\
step wave\n\
\x20 distribute item in items\n\
\n\
step build\n\
\x20 risky(item: item) -> note\n\
\n\
\x20 on failure\n\
\x20   route gather\n\
\n\
step gather\n\
\x20 collect note -> notes\n\
\x20 \"done\" |> route done\n";
    assert_error_at(
        source,
        "not definitely assigned on every success path",
        "note -> notes",
    )
}

#[test]
fn compensation_bindings_before_the_failure_route_do_flow() -> TestResult {
    let source = "//! The compensation prefix rides the failure route.\n\
workflow probe\n\
\x20 input task: String\n\
\x20 outcome done: type String, route success\n\
\n\
worker w\n\
\x20 action work(item: String) -> String\n\
\x20 action risky(item: String) -> String\n\
\n\
step produce\n\
\x20 risky(item: task) -> value\n\
\n\
\x20 on failure\n\
\x20   work(item: \"cleanup\") -> salvage\n\
\x20   route recover\n\
\n\
\x20 outcome ok: otherwise, route done(value)\n\
\n\
step recover\n\
\x20 work(item: salvage) -> shown\n\
\x20 shown |> route done\n";
    assert_clean(source)
}

// ---------------------------------------------------------------------
// Blocker 2 — sibling-substep graphs get the full cycle/visits analysis
// ---------------------------------------------------------------------

fn substep_cycle_doc(second_tail: &str) -> String {
    format!(
        "//! Sibling-substep cycle probe.\n\
         workflow probe\n\
         \x20 input task: String\n\
         \x20 outcome done: type String, route success\n\
         \n\
         worker w\n\
         \x20 action work(item: String) -> String\n\
         \n\
         step parent\n\
         \x20 step first\n\
         \x20   work(item: task) -> a\n\
         \n\
         \x20   outcome go: otherwise, route second\n\
         \n\
         \x20 step second\n\
         \x20   work(item: a) -> b\n\
         \n\
         {second_tail}\
         \n\
         \x20 outcome finish: otherwise, route done(\"over\")\n"
    )
}

#[test]
fn an_unbounded_sibling_substep_cycle_is_rejected() -> TestResult {
    let source = substep_cycle_doc(
        "\x20   outcome back: when b == \"\", route first\n\
         \x20   outcome up: otherwise, route finish\n",
    );
    let (line, column) = line_col_of_last(&source, "first\n")?;
    assert_error_at_position(&source, "cycle with no bound", line, column)?;
    assert_error_at_position(&source, "max … visits", line, column)
}

#[test]
fn a_bounded_sibling_substep_cycle_is_accepted() -> TestResult {
    let source = substep_cycle_doc(
        "\x20   outcome back: when b == \"\" and visits < 2, route first\n\
         \x20   outcome up: otherwise, route finish\n\
         \x20   max 2 visits\n",
    );
    assert_clean(&source)
}

#[test]
fn a_self_routing_substep_without_a_bound_is_rejected() -> TestResult {
    let source = "//! Self-routing substep.\n\
workflow probe\n\
\x20 input task: String\n\
\x20 outcome done: type String, route success\n\
\n\
worker w\n\
\x20 action work(item: String) -> String\n\
\n\
step parent\n\
\x20 step only\n\
\x20   work(item: task) -> a\n\
\n\
\x20   outcome again: when a == \"\", route only\n\
\x20   outcome up: otherwise, route finish\n\
\n\
\x20 outcome finish: otherwise, route done(\"over\")\n";
    assert_error_at_position(
        source,
        "cycle with no bound",
        line_col_of_last(source, "only\n\x20   outcome up")?.0,
        line_col_of_last(source, "only\n\x20   outcome up")?.1,
    )
}

#[test]
fn a_substep_visits_bound_is_type_checked() -> TestResult {
    let source = "//! Substep bound typing.\n\
workflow probe\n\
\x20 input task: String\n\
\x20 outcome done: type String, route success\n\
\n\
worker w\n\
\x20 action work(item: String) -> String\n\
\n\
step parent\n\
\x20 step only\n\
\x20   work(item: task) -> a\n\
\n\
\x20   outcome again: when a == \"\", route only\n\
\x20   outcome up: otherwise, route finish\n\
\x20   max task visits\n\
\n\
\x20 outcome finish: otherwise, route done(\"over\")\n";
    assert_error_at(source, "needs an Int bound", "task visits")
}

// ---------------------------------------------------------------------
// Major 3 — a visits bound must intersect EVERY directed cycle
// ---------------------------------------------------------------------

fn residual_cycle_doc(cycle_node_tail: &str) -> String {
    format!(
        "//! Residual-cycle probe: bounded <-> branch <-> cycle_node.\n\
         workflow probe\n\
         \x20 input task: String\n\
         \x20 outcome done: type String, route success\n\
         \n\
         worker w\n\
         \x20 action work(item: String) -> String\n\
         \n\
         step bounded\n\
         \x20 work(item: task) -> seed\n\
         \n\
         \x20 outcome go: otherwise, route branch\n\
         \x20 max 1 visits\n\
         \n\
         step branch\n\
         \x20 work(item: seed) -> pick\n\
         \n\
         \x20 outcome to_bounded: when pick == \"a\", route bounded\n\
         \x20 outcome to_cycle: when pick == \"b\", route cycle_node\n\
         \x20 outcome out: otherwise, route done(pick)\n\
         \n\
         step cycle_node\n\
         \x20 work(item: pick) -> next\n\
         \n\
         \x20 outcome again: otherwise, route branch\n\
         {cycle_node_tail}"
    )
}

#[test]
fn a_sub_cycle_avoiding_the_bounded_vertex_is_rejected() -> TestResult {
    // branch -> cycle_node -> branch never passes through `bounded`, so
    // its `max 1 visits` bounds nothing on that loop.
    let source = residual_cycle_doc("");
    let (line, column) = line_col_of_last(&source, "branch\n")?;
    assert_error_at_position(&source, "cycle with no bound", line, column)
}

#[test]
fn bounds_covering_every_overlapping_cycle_check_clean() -> TestResult {
    let source = residual_cycle_doc("\x20 max 2 visits\n");
    assert_clean(&source)
}

// ---------------------------------------------------------------------
// Major 4 — incompatible per-path types no longer collapse to Unknown
// ---------------------------------------------------------------------

#[test]
fn incompatible_per_path_collected_types_are_rejected() -> TestResult {
    let source = "//! Same name, two types, one collect.\n\
workflow probe\n\
\x20 input items: [String]\n\
\x20 outcome done: type String, route success\n\
\n\
type Alt { n: Int }\n\
\n\
worker w\n\
\x20 action sort_out(item: String) -> String\n\
\x20 action make_s(item: String) -> String\n\
\x20 action make_a(item: String) -> Alt\n\
\n\
step wave\n\
\x20 distribute item in items\n\
\n\
step sift\n\
\x20 sort_out(item: item) -> kind\n\
\n\
\x20 outcome to_s: when kind == \"s\", route build_s\n\
\x20 outcome to_a: otherwise, route build_a\n\
\n\
step build_s after sift\n\
\x20 make_s(item: item) -> value\n\
\x20 route gather\n\
\n\
step build_a after sift\n\
\x20 make_a(item: item) -> value\n\
\x20 route gather\n\
\n\
step gather\n\
\x20 collect value -> values\n\
\x20 \"done\" |> route done\n";
    assert_error_at(
        source,
        "one type per binding name",
        "value\n\x20 route gather\n\nstep gather",
    )
}

// ---------------------------------------------------------------------
// Major 5 — subflow parameters cannot shadow document consts
// ---------------------------------------------------------------------

#[test]
fn a_subflow_parameter_cannot_shadow_a_const() -> TestResult {
    let source = "//! Const/parameter shadowing.\n\
workflow probe\n\
\x20 input task: String\n\
\x20 outcome done: type String, route success\n\
\n\
const item = \"fixed\"\n\
\n\
worker w\n\
\x20 action work(item: String) -> String\n\
\n\
subflow inner(item: String)\n\
\x20 outcome out: type String\n\
\x20 step run\n\
\x20   work(item: item) -> note\n\
\x20   note |> route out\n\
\n\
step call_it\n\
\x20 inner(item: task) -> got\n\
\n\
step finish_up\n\
\x20 got |> route done\n";
    // Anchored on the textually second declaration: the parameter.
    assert_error_at(
        source,
        "collides with a parameter of subflow `inner`",
        "item: String)\n\x20 outcome out",
    )
}

// ---------------------------------------------------------------------
// Minor 6 — same-named subflow outcomes keep their declaration targets
// ---------------------------------------------------------------------

#[test]
fn same_named_subflow_outcomes_resolve_to_their_own_declarations() -> TestResult {
    let source = "//! Two subflows, one outcome name.\n\
workflow probe\n\
\x20 input task: String\n\
\x20 outcome done: type String, route success\n\
\n\
worker w\n\
\x20 action work(item: String) -> String\n\
\n\
subflow alpha(item: String)\n\
\x20 outcome out: type String\n\
\x20 step run\n\
\x20   work(item: item) -> note_a\n\
\x20   note_a |> route out\n\
\n\
subflow beta(item: String)\n\
\x20 outcome out: type String\n\
\x20 step run\n\
\x20   work(item: item) -> note_b\n\
\x20   note_b |> route out\n\
\n\
step first\n\
\x20 alpha(item: task) -> got_a\n\
\n\
step second\n\
\x20 beta(item: got_a) -> got_b\n\
\n\
step finish_up\n\
\x20 got_b |> route done\n";
    let document = parse(source)?;
    let analysis = analyze(&document);
    assert!(
        analysis.diagnostics().is_empty(),
        "must check clean: {:#?}",
        analysis.diagnostics()
    );
    for (needle, subflow_index) in [("out\n\nsubflow beta", 0), ("out\n\nstep first", 1)] {
        let (line, column) = line_col_of(source, needle)?;
        let info = analysis
            .iter()
            .find(|info| info.span.line == line && info.span.column == column)
            .ok_or("no semantic facts on the route reference")?;
        let declaration = info
            .declaration
            .as_ref()
            .ok_or("route reference lost its declaration")?;
        let expected = document
            .subflows
            .get(subflow_index)
            .ok_or("missing subflow")?
            .outcome
            .name_span;
        assert_eq!(
            declaration.span, expected,
            "route `out` must resolve to its own subflow's outcome"
        );
    }
    Ok(())
}
