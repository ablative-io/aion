//! Flow-vocabulary B2 shape: lossless canonical printing of the full
//! rev-3 surface (comments included) and the semantic index's step kinds,
//! subflow declarations, and collect result types.

use std::error::Error;

use aion_awl::semantic::{DeclarationKind, StepKind, analyze};
use aion_awl::{check, parse, print};

type TestResult = Result<(), Box<dyn Error>>;

fn line_col_of(source: &str, needle: &str) -> Result<(usize, usize), Box<dyn Error>> {
    let start = source
        .find(needle)
        .ok_or_else(|| format!("needle {needle:?} not found"))?;
    let prefix = &source[..start];
    let line = prefix.matches('\n').count() + 1;
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    Ok((line, start - line_start + 1))
}

/// A canonical document exercising the full rev-3 flow surface at once,
/// comments included: a subflow with docs and a trailing comment, a
/// distribute region, a subflow-call step, a tolerant collect, a visits
/// bound with the `visits` builtin in a guard, a value route payload, and a
/// pure decision step.
const SHAPE_DOC: &str = "//! Shape proof.\n\
workflow shape_proof\n\
\x20 input items: [String]\n\
\x20 outcome done: type String, route success\n\
\n\
worker w\n\
\x20 action work(item: String) -> String // the one action\n\
\n\
/// Develop one item.\n\
subflow dev_item(item: String) // inline container\n\
\x20 outcome out: type String\n\
\n\
\x20 // per-item track\n\
\x20 step develop\n\
\x20   work(item: item) -> note\n\
\n\
\x20   outcome redo: when note == \"\" and visits < 3, route develop\n\
\x20   outcome ok: otherwise, route out(note) // value payload\n\
\x20   max 3 visits\n\
\n\
step wave\n\
\x20 distribute item in items // opens the region\n\
\n\
step build\n\
\x20 dev_item(item: item) -> verdict\n\
\n\
step gather\n\
\x20 collect verdict? -> results // tolerant: [String?]\n\
\n\
step decide\n\
\x20 outcome finish: when results is empty, route done(\"empty\")\n\
\x20 outcome full: otherwise, route done(\"full\")\n";

// ---------------------------------------------------------------------
// Printer: lossless round-trip and idempotency, comments included
// ---------------------------------------------------------------------

#[test]
fn the_shape_document_round_trips_byte_identically() -> TestResult {
    let tree = parse(SHAPE_DOC)?;
    let errors = check(&tree);
    assert!(errors.is_empty(), "document must check clean: {errors:#?}");
    let printed = print(&tree);
    assert_eq!(printed, SHAPE_DOC);
    assert_eq!(parse(&printed)?, tree);
    assert_eq!(print(&parse(&printed)?), printed);
    Ok(())
}

#[test]
fn a_sequence_region_with_a_strict_collect_round_trips() -> TestResult {
    let source = "//! Sequence proof.\n\
workflow seq_proof\n\
\x20 input items: [String]\n\
\x20 outcome done: type String, route success\n\
\n\
worker w\n\
\x20 action work(item: String) -> String\n\
\n\
step ordered\n\
\x20 sequence item in items\n\
\n\
step build\n\
\x20 work(item: item) -> note\n\
\n\
step gather\n\
\x20 collect note -> notes // strict: [String]\n\
\x20 \"done\" |> route done\n";
    let tree = parse(source)?;
    assert!(check(&tree).is_empty(), "document must check clean");
    assert_eq!(print(&tree), source);
    assert_eq!(parse(&print(&tree))?, tree);
    Ok(())
}

// ---------------------------------------------------------------------
// Semantic index: step kinds, subflow declarations, collect result types
// ---------------------------------------------------------------------

#[test]
fn every_step_carries_its_kind_in_the_semantic_index() -> TestResult {
    let document = parse(SHAPE_DOC)?;
    let analysis = analyze(&document);
    assert!(analysis.diagnostics().is_empty());
    let kinds: Vec<(&str, StepKind, Option<&str>)> = analysis
        .step_kinds()
        .iter()
        .map(|step| (step.name.as_str(), step.kind, step.subflow.as_deref()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("wave", StepKind::Distribute, None),
            ("build", StepKind::SubflowCall, None),
            ("gather", StepKind::Collect, None),
            ("decide", StepKind::Decision, None),
            ("develop", StepKind::Plain, Some("dev_item")),
        ]
    );
    Ok(())
}

#[test]
fn every_step_carries_its_region_membership_in_the_semantic_index() -> TestResult {
    let document = parse(SHAPE_DOC)?;
    let analysis = analyze(&document);
    assert!(analysis.diagnostics().is_empty());
    let regions: Vec<(&str, Option<&str>)> = analysis
        .step_kinds()
        .iter()
        .map(|step| (step.name.as_str(), step.region.as_deref()))
        .collect();
    assert_eq!(
        regions,
        vec![
            ("wave", None),
            ("build", Some("wave")),
            ("gather", None),
            ("decide", None),
            ("develop", None),
        ]
    );
    Ok(())
}

#[test]
fn sequence_steps_classify_as_sequence() -> TestResult {
    let source = "//! Kinds.\n\
workflow t\n\
\x20 input items: [String]\n\
\x20 outcome done: type String, route success\n\
\n\
worker w\n\
\x20 action work(item: String) -> String\n\
\n\
step ordered\n\
\x20 sequence item in items\n\
\n\
step build\n\
\x20 work(item: item) -> note\n\
\n\
step gather\n\
\x20 collect note -> notes\n\
\x20 \"done\" |> route done\n";
    let document = parse(source)?;
    let analysis = analyze(&document);
    assert!(analysis.diagnostics().is_empty());
    let ordered = analysis
        .step_kinds()
        .iter()
        .find(|step| step.name == "ordered")
        .ok_or("missing step kind for `ordered`")?;
    assert_eq!(ordered.kind, StepKind::Sequence);
    assert_eq!(ordered.kind.as_str(), "sequence");
    Ok(())
}

#[test]
fn the_subflow_declaration_reaches_the_semantic_index() -> TestResult {
    let document = parse(SHAPE_DOC)?;
    let analysis = analyze(&document);
    let subflow = document.subflows.first().ok_or("expected a subflow")?;
    let info = analysis
        .info_for_span(subflow.name_span)
        .ok_or("expected semantic facts on the subflow name")?;
    let declaration = info
        .declaration
        .as_ref()
        .ok_or("expected a declaration on the subflow name")?;
    assert_eq!(declaration.kind, DeclarationKind::Subflow);
    assert_eq!(declaration.name, "dev_item");
    assert_eq!(
        declaration.documentation.as_deref(),
        Some("Develop one item.")
    );
    Ok(())
}

#[test]
fn collect_results_type_strict_and_tolerant_forms() -> TestResult {
    // Tolerant: `[String?]`, slot per item.
    let document = parse(SHAPE_DOC)?;
    let analysis = analyze(&document);
    let (line, column) = line_col_of(SHAPE_DOC, "results // tolerant")?;
    let info = analysis
        .iter()
        .find(|info| info.span.line == line && info.span.column == column)
        .ok_or("no semantic facts on the tolerant collect binding")?;
    assert_eq!(info.ty.as_deref(), Some("[String?]"));
    // Strict: `[String]`.
    let source = "//! Strict.\n\
workflow t\n\
\x20 input items: [String]\n\
\x20 outcome done: type String, route success\n\
\n\
worker w\n\
\x20 action work(item: String) -> String\n\
\n\
step wave\n\
\x20 distribute item in items\n\
\n\
step build\n\
\x20 work(item: item) -> note\n\
\n\
step gather\n\
\x20 collect note -> notes\n\
\x20 \"done\" |> route done\n";
    let document = parse(source)?;
    let analysis = analyze(&document);
    assert!(analysis.diagnostics().is_empty());
    let (line, column) = line_col_of(source, "notes\n")?;
    let info = analysis
        .iter()
        .find(|info| info.span.line == line && info.span.column == column)
        .ok_or("no semantic facts on the strict collect binding")?;
    assert_eq!(info.ty.as_deref(), Some("[String]"));
    Ok(())
}
