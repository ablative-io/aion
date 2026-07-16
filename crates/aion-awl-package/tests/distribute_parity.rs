//! Direct-path distribute parity at the archive seam: a multi-step
//! `distribute` document compiles through the DIRECT bytecode path into an
//! archive whose manifest carries the implicit per-item children, with the
//! exact hygienic identities the Gleam path synthesizes for the same
//! document — so a package produced by either path exposes interchangeable
//! route identities.

use aion_awl_package::compile_and_assemble_awl;
use aion_package::{ExtractionLimits, Package};

/// The B5 engine-proof document: a three-step parallel region with a
/// tolerant collect.
const MULTI_STEP: &str = r#"//! Direct-path parity proof document.
workflow direct_parity_probe
  input items: [String]
  outcome done: type Done, route success

type Done { first: String, middle_present: Bool, third: String }

worker proof
  action stage_one(item: String) -> String
  action stage_two(item: String) -> String

step fan
  distribute item in items

step first
  stage_one(item: item) -> prepared

step second
  stage_two(item: prepared) -> result

step gather
  collect result? -> results
  route done(first: "unprojected", middle_present: true, third: "unprojected")
"#;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The direct archive's manifest lists the implicit child with the same
/// deterministic identity, entry function, schemas, and internal marking the
/// Gleam path's synthesized artifact carries for the same document.
#[test]
fn direct_archive_carries_the_implicit_child_with_gleam_path_identity() -> TestResult {
    let prepared = compile_and_assemble_awl(MULTI_STEP, std::path::Path::new("."))?;
    let package =
        Package::load_from_bytes(prepared.archive.clone(), ExtractionLimits::unbounded())?;
    let entries = &package.manifest().additional_workflows;
    let [entry] = entries.as_slice() else {
        return Err(format!("expected exactly one child entry, got {}", entries.len()).into());
    };

    // The Gleam path's synthesized identity for the same document.
    let document = aion_awl::parse(MULTI_STEP)?;
    let artifact = aion_awl::emit_artifact(&document)?;
    let [reference] = artifact.synthesized_workflows.as_slice() else {
        return Err("the Gleam path must synthesize exactly one child".into());
    };

    assert_eq!(entry.workflow_type, reference.workflow_type);
    assert_eq!(
        entry.workflow_type,
        "aion_internal_awl_child_direct_parity_probe_fan_0"
    );
    assert_eq!(entry.entry_module, reference.entry_module);
    assert_eq!(entry.entry_function, reference.entry_function);
    assert_eq!(entry.input_schema, reference.input_schema);
    assert_eq!(entry.output_schema, reference.output_schema);
    assert_eq!(entry.timeout.as_secs(), reference.timeout_seconds);
    assert!(entry.internal, "implicit children are package-internal");

    // The compile seam's own entries match too (the packaging input).
    assert_eq!(
        prepared.compiled.synthesized_workflows.as_slice(),
        artifact.synthesized_workflows.as_slice()
    );
    Ok(())
}

/// Determinism: the same document yields byte-identical archives (the
/// multi-entry manifest folds into the same stamped bytes).
#[test]
fn direct_distribute_archive_is_deterministic() -> TestResult {
    let first = compile_and_assemble_awl(MULTI_STEP, std::path::Path::new("."))?;
    let second = compile_and_assemble_awl(MULTI_STEP, std::path::Path::new("."))?;
    assert_eq!(first.archive, second.archive);
    Ok(())
}

/// A single-activity distribute stays a combinator fan-out: no implicit
/// child is synthesized, so the manifest carries no additional entries (the
/// dispatch gate is shared with the Gleam path).
#[test]
fn single_activity_distribute_synthesizes_no_child() -> TestResult {
    let source = r"//! Single-activity track: combinator fan-out, no child.
workflow direct_parity_single
  input items: [String]
  outcome done: type Int, route success

worker proof
  action stage_one(item: String) -> String

step fan
  distribute item in items

step only
  stage_one(item: item) -> result

step gather
  collect result -> results
  results |> count |> route done
";
    let prepared = compile_and_assemble_awl(source, std::path::Path::new("."))?;
    let package = Package::load_from_bytes(prepared.archive, ExtractionLimits::unbounded())?;
    assert!(package.manifest().additional_workflows.is_empty());
    assert!(prepared.compiled.synthesized_workflows.is_empty());
    Ok(())
}
