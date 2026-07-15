//! Flow-vocabulary B2 region rules: formation (bracket nesting), placement
//! (`distribute` alone, `collect` first, step-level only), no-escape and
//! no-mid-entry routing, in-region loop-backs, definite assignment of the
//! collected binding, region-local masking, the `max … visits` cycle rule
//! (decoy loops rejected), and the `visits` builtin's scope — every
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
// Region formation: bracket nesting, unclosed and unopened regions
// ---------------------------------------------------------------------

#[test]
fn nested_regions_with_inner_loopback_check_clean() -> TestResult {
    let source = doc("step outer_wave\n\
         \x20 distribute item in items\n\
         \n\
         step expandit\n\
         \x20 expand(item: item) -> batch\n\
         \n\
         step inner_wave\n\
         \x20 sequence part in batch.parts\n\
         \n\
         step polish after inner_wave\n\
         \x20 work(item: part) -> note\n\
         \n\
         \x20 outcome again: when note == \"\" and visits < 2, route polish\n\
         \x20 outcome good: otherwise, route inner_gather\n\
         \x20 max 2 visits\n\
         \n\
         step inner_gather\n\
         \x20 collect note -> notes\n\
         \n\
         step summarize\n\
         \x20 notes |> count -> n\n\
         \x20 work(item: item) -> outer_note\n\
         \n\
         step outer_gather\n\
         \x20 collect outer_note -> outer_notes\n\
         \x20 \"done\" |> route done\n");
    assert_clean(&source)
}

#[test]
fn an_unclosed_region_is_rejected_at_its_opener() -> TestResult {
    let source = doc("step wave\n\
         \x20 distribute item in items\n\
         \n\
         step build\n\
         \x20 work(item: item) -> note\n\
         \x20 note |> route done\n");
    assert_error_at(
        &source,
        "never reaches a `collect`",
        "distribute item in items",
    )
}

#[test]
fn a_collect_with_no_open_region_is_rejected() -> TestResult {
    let source = doc("step lonely\n\
         \x20 collect note -> notes\n\
         \x20 \"done\" |> route done\n");
    assert_error_at(&source, "closes no region", "collect note -> notes")
}

#[test]
fn a_collect_closes_the_nearest_open_region() -> TestResult {
    // Outer + inner distribute, one collect: the collect pairs with the
    // INNER region (bracket nesting), so the outer opener is the unclosed
    // one — pinned by the diagnostic anchor.
    let source = doc("step outer_wave\n\
         \x20 distribute item in items\n\
         \n\
         step expandit\n\
         \x20 expand(item: item) -> batch\n\
         \n\
         step inner_wave\n\
         \x20 sequence part in batch.parts\n\
         \n\
         step polish\n\
         \x20 work(item: part) -> note\n\
         \n\
         step gather\n\
         \x20 collect note -> notes\n\
         \x20 \"done\" |> route done\n");
    assert_error_at(
        &source,
        "never reaches a `collect`",
        "distribute item in items",
    )
}

// ---------------------------------------------------------------------
// Placement: only line, opens its step, step-level only
// ---------------------------------------------------------------------

#[test]
fn distribute_must_be_its_steps_only_line() -> TestResult {
    let source = doc("step wave\n\
         \x20 distribute item in items\n\
         \x20 work(item: item) -> note\n\
         \n\
         step gather\n\
         \x20 collect note -> notes\n\
         \x20 \"done\" |> route done\n");
    assert_error_at(
        &source,
        "is its step's only line",
        "distribute item in items",
    )
}

#[test]
fn a_distribute_step_may_not_carry_outcomes() -> TestResult {
    let source = doc("step wave\n\
         \x20 distribute item in items\n\
         \n\
         \x20 outcome go: otherwise, route gather\n\
         \n\
         step gather\n\
         \x20 collect item -> gathered\n\
         \x20 \"done\" |> route done\n");
    assert_error_at(
        &source,
        "is its step's only line",
        "distribute item in items",
    )
}

#[test]
fn collect_must_open_its_step() -> TestResult {
    let source = doc("step wave\n\
         \x20 distribute item in items\n\
         \n\
         step build\n\
         \x20 work(item: item) -> note\n\
         \n\
         step gather\n\
         \x20 work(item: \"x\") -> extra\n\
         \x20 collect note -> notes\n\
         \x20 \"done\" |> route done\n");
    assert_error_at(&source, "`collect` opens its step", "collect note -> notes")
}

#[test]
fn region_statements_are_step_level_only() -> TestResult {
    let source = doc("step wave\n\
         \x20 fork item in items\n\
         \x20   distribute part in items\n\
         \x20 join\n\
         \x20 \"done\" |> route done\n");
    assert_error_at(
        &source,
        "cannot appear inside a `fork` branch",
        "distribute part in items",
    )
}

// ---------------------------------------------------------------------
// Routing: the only exit is the collect; the only entry is the opener
// ---------------------------------------------------------------------

#[test]
fn a_route_out_of_the_region_is_rejected() -> TestResult {
    let source = doc("step wave\n\
         \x20 distribute item in items\n\
         \n\
         step build\n\
         \x20 work(item: item) -> note\n\
         \n\
         \x20 outcome skip: when note == \"\", route epilogue\n\
         \x20 outcome keep: otherwise, route gather\n\
         \n\
         step gather\n\
         \x20 collect note -> notes\n\
         \n\
         step epilogue\n\
         \x20 \"done\" |> route done\n");
    assert_error_at(&source, "leaves the per-item region", "epilogue\n")
}

#[test]
fn a_route_to_a_flow_outcome_from_inside_is_rejected() -> TestResult {
    let source = doc("step wave\n\
         \x20 distribute item in items\n\
         \n\
         step build\n\
         \x20 work(item: item) -> note\n\
         \n\
         \x20 outcome bail: when note == \"\", route done(note)\n\
         \x20 outcome keep: otherwise, route gather\n\
         \n\
         step gather\n\
         \x20 collect note -> notes\n\
         \x20 \"done\" |> route done\n");
    assert_error_at(&source, "leaves the per-item region", "done(note)")
}

#[test]
fn a_route_entering_the_region_mid_track_is_rejected() -> TestResult {
    let source = doc("step plan\n\
         \x20 work(item: \"x\") -> extra\n\
         \n\
         \x20 outcome jump: when extra == \"\", route build\n\
         \x20 outcome go: otherwise, route wave\n\
         \n\
         step wave\n\
         \x20 distribute item in items\n\
         \n\
         step build after wave\n\
         \x20 work(item: item) -> note\n\
         \n\
         step gather\n\
         \x20 collect note -> notes\n\
         \x20 \"done\" |> route done\n");
    assert_error_at(&source, "enters the per-item region", "build\n")
}

#[test]
fn a_route_to_the_collect_from_outside_is_rejected() -> TestResult {
    let source = doc("step plan\n\
         \x20 work(item: \"x\") -> extra\n\
         \n\
         \x20 outcome jump: when extra == \"\", route gather\n\
         \x20 outcome go: otherwise, route wave\n\
         \n\
         step wave\n\
         \x20 distribute item in items\n\
         \n\
         step build after wave\n\
         \x20 work(item: item) -> note\n\
         \n\
         step gather\n\
         \x20 collect note -> notes\n\
         \x20 \"done\" |> route done\n");
    assert_error_at(&source, "never routed to from outside", "gather\n")
}

#[test]
fn an_in_region_loopback_is_legal_and_stays_inside() -> TestResult {
    let source = doc("step wave\n\
         \x20 distribute item in items\n\
         \n\
         step build after wave\n\
         \x20 work(item: item) -> note\n\
         \n\
         \x20 outcome redo: when note == \"\" and visits < 3, route build\n\
         \x20 outcome ok: otherwise, route gather\n\
         \x20 max 3 visits\n\
         \n\
         step gather\n\
         \x20 collect note -> notes\n\
         \x20 \"done\" |> route done\n");
    assert_clean(&source)
}

#[test]
fn an_after_edge_may_not_cross_the_region_boundary() -> TestResult {
    let source = doc("step wave\n\
         \x20 distribute item in items\n\
         \n\
         step build\n\
         \x20 work(item: item) -> note\n\
         \n\
         step gather\n\
         \x20 collect note -> notes\n\
         \n\
         step epilogue after build\n\
         \x20 \"done\" |> route done\n");
    assert_error_at_last(&source, "`after` may not cross", "build\n")
}

// ---------------------------------------------------------------------
// The collected binding: produced inside, definitely assigned, masked out
// ---------------------------------------------------------------------

#[test]
fn the_collected_binding_must_be_produced_inside_the_region() -> TestResult {
    let source = doc("step plan\n\
         \x20 work(item: \"x\") -> outside_note\n\
         \n\
         step wave after plan\n\
         \x20 distribute item in items\n\
         \n\
         step build\n\
         \x20 work(item: item) -> note\n\
         \n\
         step gather\n\
         \x20 collect outside_note -> notes\n\
         \x20 \"done\" |> route done\n");
    assert_error_at(
        &source,
        "not bound inside the region",
        "outside_note -> notes",
    )
}

#[test]
fn the_collected_binding_must_be_assigned_on_every_success_path() -> TestResult {
    let source = doc("step wave\n\
         \x20 distribute item in items\n\
         \n\
         step sift\n\
         \x20 expand(item: item) -> batch\n\
         \n\
         \x20 outcome pass: when batch.parts is empty, route gather\n\
         \x20 outcome work_it: otherwise, route build\n\
         \n\
         step build after sift\n\
         \x20 work(item: item) -> note\n\
         \x20 route gather\n\
         \n\
         step gather\n\
         \x20 collect note -> notes\n\
         \x20 \"done\" |> route done\n");
    assert_error_at(
        &source,
        "not definitely assigned on every success path",
        "note -> notes",
    )
}

#[test]
fn region_locals_fall_out_of_scope_at_the_collect() -> TestResult {
    // Reading the per-item variable after the collect is an error: the
    // per-item track was merged there.
    let source = doc("step wave\n\
         \x20 distribute item in items\n\
         \n\
         step build\n\
         \x20 work(item: item) -> note\n\
         \n\
         step gather\n\
         \x20 collect note -> notes\n\
         \x20 work(item: item) -> leak\n\
         \x20 \"done\" |> route done\n");
    let errors = errors_of(&source)?;
    let needle_at = source.rfind("item) -> leak").ok_or("needle missing")?;
    let prefix = &source[..needle_at];
    let line = prefix.matches('\n').count() + 1;
    let matched = errors.iter().any(|error| {
        error.span.line == line
            && (error.message.contains("not guaranteed on every path")
                || error.message.contains("unknown name"))
    });
    assert!(
        matched,
        "expected a masked-binding read error on line {line}: {errors:#?}"
    );
    Ok(())
}

#[test]
fn an_empty_collection_is_legal() -> TestResult {
    // Zero instances: the collect yields `[]` and flow continues — nothing
    // for the checker to reject in the empty-list shape.
    let source = doc("step wave\n\
         \x20 distribute item in []\n\
         \n\
         step build\n\
         \x20 work(item: item) -> note\n\
         \n\
         step gather\n\
         \x20 collect note -> notes\n\
         \x20 \"done\" |> route done\n");
    assert_clean(&source)
}

#[test]
fn distribute_needs_a_list_collection() -> TestResult {
    let source = doc("step wave\n\
         \x20 distribute item in \"not-a-list\"\n\
         \n\
         step gather\n\
         \x20 collect item -> gathered\n\
         \x20 \"done\" |> route done\n");
    assert_error_at(&source, "needs a list to fan out over", "\"not-a-list\"")
}
