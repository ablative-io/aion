//! Flow-vocabulary B2 subflows: parser diagnostics for the new grammar
//! and the subflow anatomy/invocation rules — one outcome, no capture,
//! its-own-step placement, no recursion, value payload contracts — every
//! diagnostic asserted at its line and column.

use std::error::Error;

use aion_awl::semantic::analyze;
use aion_awl::{CheckError, check, parse};

type TestResult = Result<(), Box<dyn Error>>;

fn errors_of(source: &str) -> Result<Vec<CheckError>, Box<dyn Error>> {
    Ok(check(&parse(source)?))
}

fn line_col_of(source: &str, needle: &str) -> Result<(usize, usize), Box<dyn Error>> {
    let start = source
        .find(needle)
        .ok_or_else(|| format!("needle {needle:?} not found"))?;
    let prefix = &source[..start];
    let line = prefix.matches('\n').count() + 1;
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    Ok((line, start - line_start + 1))
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

// ---------------------------------------------------------------------
// Parser diagnostics for the new grammar
// ---------------------------------------------------------------------

#[test]
fn a_subflow_declares_exactly_one_outcome() -> TestResult {
    let source = "//! Two outcomes.\n\
workflow t\n\
\x20 outcome done: type String, route success\n\
\n\
subflow s(item: String)\n\
\x20 outcome out: type String\n\
\x20 outcome extra: type String\n\
\x20 step run\n\
\x20   item |> route out\n";
    let Err(error) = parse(source) else {
        return Err("expected a parse error".into());
    };
    assert!(
        error.message.contains("second outcome"),
        "unexpected message: {}",
        error.message
    );
    assert_eq!((error.span.line, error.span.column), (7, 11));
    Ok(())
}

#[test]
fn a_subflow_without_an_outcome_is_refused() -> TestResult {
    let source = "//! No outcome.\n\
workflow t\n\
\x20 outcome done: type String, route success\n\
\n\
subflow s(item: String)\n\
\x20 step run\n\
\x20   item |> route out\n";
    let Err(error) = parse(source) else {
        return Err("expected a parse error".into());
    };
    assert!(
        error.message.contains("declares no outcome"),
        "unexpected message: {}",
        error.message
    );
    assert_eq!(error.span.line, 5);
    Ok(())
}

#[test]
fn a_subflow_outcome_carries_no_route() -> TestResult {
    let source = "//! Routed outcome.\n\
workflow t\n\
\x20 outcome done: type String, route success\n\
\n\
subflow s(item: String)\n\
\x20 outcome out: type String, route success\n\
\x20 step run\n\
\x20   item |> route out\n";
    let Err(error) = parse(source) else {
        return Err("expected a parse error".into());
    };
    assert!(
        error.message.contains("carries no route"),
        "unexpected message: {}",
        error.message
    );
    assert_eq!((error.span.line, error.span.column), (6, 27));
    Ok(())
}

#[test]
fn a_step_level_max_must_end_in_visits() -> TestResult {
    let source = "//! Bad max.\n\
workflow t\n\
\x20 outcome done: type String, route success\n\
\n\
step run\n\
\x20 \"x\" |> route done\n\
\x20 max 3\n";
    let Err(error) = parse(source) else {
        return Err("expected a parse error".into());
    };
    assert!(
        error.message.contains("ends in `visits`"),
        "unexpected message: {}",
        error.message
    );
    assert_eq!((error.span.line, error.span.column), (7, 3));
    Ok(())
}

#[test]
fn collect_requires_its_result_binding() -> TestResult {
    let source = "//! No arrow.\n\
workflow t\n\
\x20 outcome done: type String, route success\n\
\n\
step gather\n\
\x20 collect note\n";
    let Err(error) = parse(source) else {
        return Err("expected a parse error".into());
    };
    assert!(
        error.message.contains("binds the gathered collection"),
        "unexpected message: {}",
        error.message
    );
    assert_eq!((error.span.line, error.span.column), (6, 3));
    Ok(())
}

// ---------------------------------------------------------------------
// Subflow rules: anatomy, capture, placement, recursion
// ---------------------------------------------------------------------

fn subflow_doc(body: &str) -> String {
    format!(
        "//! Subflow rules.\n\
         workflow t\n\
         \x20 input task: String\n\
         \x20 outcome done: type String, route success\n\
         \n\
         worker w\n\
         \x20 action work(item: String) -> String\n\
         \n\
         {body}"
    )
}

#[test]
fn a_subflow_captures_nothing_from_the_enclosing_flow() -> TestResult {
    let source = subflow_doc(
        "subflow s(item: String)\n\
         \x20 outcome out: type String\n\
         \x20 step run\n\
         \x20   work(item: task) -> note\n\
         \x20   note |> route out\n\
         \n\
         step call_it\n\
         \x20 s(item: task) -> got\n\
         \n\
         step finish_up\n\
         \x20 got |> route done\n",
    );
    // `task` is a workflow input, invisible inside the subflow.
    assert_error_at(&source, "unknown name `task`", "task) -> note")
}

#[test]
fn document_consts_stay_visible_inside_a_subflow() -> TestResult {
    let source = subflow_doc(
        "const seed = \"seeded\"\n\
         \n\
         subflow s(item: String)\n\
         \x20 outcome out: type String\n\
         \x20 step run\n\
         \x20   work(item: seed + item) -> note\n\
         \x20   note |> route out\n\
         \n\
         step call_it\n\
         \x20 s(item: task) -> got\n\
         \n\
         step finish_up\n\
         \x20 got |> route done\n",
    );
    let errors = errors_of(&source)?;
    assert!(errors.is_empty(), "expected clean, got {errors:#?}");
    Ok(())
}

#[test]
fn a_subflow_call_binds_the_outcome_type() -> TestResult {
    let source = subflow_doc(
        "subflow s(item: String)\n\
         \x20 outcome out: type Verdict\n\
         \x20 step run\n\
         \x20   work(item: item) -> note\n\
         \x20   Verdict(text: note) |> route out\n\
         \n\
         type Verdict { text: String }\n\
         \n\
         step call_it\n\
         \x20 s(item: task) -> got\n\
         \n\
         step finish_up\n\
         \x20 got.text |> route done\n",
    );
    let document = parse(&source)?;
    let analysis = analyze(&document);
    assert!(analysis.diagnostics().is_empty(), "must check clean");
    let (line, column) = line_col_of(&source, "got\n")?;
    let info = analysis
        .iter()
        .find(|info| info.span.line == line && info.span.column == column)
        .ok_or("no semantic facts on the invocation binding")?;
    assert_eq!(info.ty.as_deref(), Some("Verdict"));
    Ok(())
}

#[test]
fn a_subflow_call_must_be_its_steps_only_statement() -> TestResult {
    let source = subflow_doc(
        "subflow s(item: String)\n\
         \x20 outcome out: type String\n\
         \x20 step run\n\
         \x20   item |> route out\n\
         \n\
         step call_it\n\
         \x20 work(item: task) -> extra\n\
         \x20 s(item: extra) -> got\n\
         \x20 got |> route done\n",
    );
    assert_error_at(&source, "must be the only statement", "s(item: extra)")
}

#[test]
fn a_subflow_call_may_not_nest_inside_a_block() -> TestResult {
    let source = subflow_doc(
        "subflow s(item: String)\n\
         \x20 outcome out: type String\n\
         \x20 step run\n\
         \x20   item |> route out\n\
         \n\
         step call_it\n\
         \x20 fork item in [task]\n\
         \x20   s(item: item) -> got\n\
         \x20 join -> all_got\n\
         \x20 \"done\" |> route done\n",
    );
    assert_error_at(
        &source,
        "cannot run inside another statement's block",
        "s(item: item)",
    )
}

#[test]
fn a_subflow_is_not_a_pipe_stage() -> TestResult {
    let source = subflow_doc(
        "subflow s(item: String)\n\
         \x20 outcome out: type String\n\
         \x20 step run\n\
         \x20   item |> route out\n\
         \n\
         step call_it\n\
         \x20 task |> s |> route done\n",
    );
    assert_error_at(&source, "not a pipe stage", "s |> route done")
}

#[test]
fn a_subflow_cannot_be_spawned() -> TestResult {
    let source = subflow_doc(
        "subflow s(item: String)\n\
         \x20 outcome out: type String\n\
         \x20 step run\n\
         \x20   item |> route out\n\
         \n\
         step call_it\n\
         \x20 spawn s(item: task)\n\
         \x20 task |> route done\n",
    );
    assert_error_at(&source, "never `spawn` it", "s(item: task)\n")
}

#[test]
fn subflow_recursion_is_rejected() -> TestResult {
    let source = subflow_doc(
        "subflow a(item: String)\n\
         \x20 outcome out: type String\n\
         \x20 step run\n\
         \x20   b(item: item) -> got\n\
         \x20   got |> route out\n\
         \n\
         subflow b(item: String)\n\
         \x20 outcome out: type String\n\
         \x20 step run\n\
         \x20   a(item: item) -> got\n\
         \x20   got |> route out\n\
         \n\
         step call_it\n\
         \x20 a(item: task) -> got\n\
         \x20 got |> route done\n",
    );
    assert_error_at(&source, "cannot recurse", "a(item: String)")
}

#[test]
fn a_value_route_payload_must_match_the_outcome_type() -> TestResult {
    let source = subflow_doc(
        "type Verdict { text: String }\n\
         \n\
         subflow s(item: String)\n\
         \x20 outcome out: type Verdict\n\
         \x20 step run\n\
         \x20   work(item: item) -> note\n\
         \n\
         \x20   outcome ok: otherwise, route out(note)\n\
         \n\
         step call_it\n\
         \x20 s(item: task) -> got\n\
         \n\
         step finish_up\n\
         \x20 got.text |> route done\n",
    );
    assert_error_at(
        &source,
        "the payload value is String, but outcome `out` carries Verdict",
        "note)\n",
    )
}
