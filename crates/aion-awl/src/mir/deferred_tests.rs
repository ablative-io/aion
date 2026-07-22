//! Refusal-ratchet tests kept separate from the broad MIR golden module.

use std::fs;
use std::path::{Path, PathBuf};

use super::{LowerError, MirModule, lower};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lower_fixture(path: &Path) -> Result<Result<MirModule, LowerError>, Box<dyn std::error::Error>> {
    let source = fs::read_to_string(path)?;
    let document = crate::parse(&source)?;
    Ok(lower(&document, path.parent()))
}

fn lower_source(source: &str) -> Result<Result<MirModule, LowerError>, Box<dyn std::error::Error>> {
    let document = crate::parse(source)?;
    Ok(lower(&document, Some(Path::new("."))))
}

/// BC-4 adversarial fixture `nested_handlers` is out-of-intersection: the
/// reference emitter accepts its `on failure` region, but the direct path does
/// not yet lower `on failure`, so it refuses with the reference class anchored
/// to the guarding step `fulfil` (line 13). Kept as a refusal ratchet so a
/// future `on failure` lowering flips this pin loudly rather than silently.
#[test]
fn bc4_nested_handlers_refuses_on_failure() -> Result<(), Box<dyn std::error::Error>> {
    let path = manifest_dir().join("tests/fixtures/rev2/loop-outcomes/valid/nested_handlers.awl");
    match lower_fixture(&path)? {
        Err(LowerError::Unsupported { shape, span }) => {
            assert_eq!(shape, "on failure", "nested_handlers refusal drifted");
            assert_eq!(span.line, 13, "nested_handlers refusal span drifted");
        }
        other => {
            return Err(format!("nested_handlers must refuse at `on failure`: {other:?}").into());
        }
    }
    Ok(())
}

/// BC-4 adversarial fixture `zero_step_workflow` is below BOTH backends' floor:
/// it checks clean but has no steps, so the direct path refuses with "no start
/// region" (and the reference emitter likewise refuses "no steps to execute").
/// It lives outside any `valid/` sweep directory precisely because neither
/// backend can produce runnable code for it; this pin anchors the direct
/// refusal at the workflow header (line 1).
#[test]
fn bc4_zero_step_workflow_refuses_no_start_region() -> Result<(), Box<dyn std::error::Error>> {
    let path = manifest_dir().join("tests/fixtures/rev2/header-types/zero_step_workflow.awl");
    match lower_fixture(&path)? {
        Err(error @ (LowerError::Message { .. } | LowerError::Planning { .. })) => {
            let text = error.to_string();
            assert!(
                text.contains("no start region"),
                "zero_step_workflow refusal drifted: {text}"
            );
        }
        other => {
            return Err(
                format!("zero_step_workflow must refuse (no start region): {other:?}").into(),
            );
        }
    }
    Ok(())
}

/// The two combined fixtures advance to their next honest refusal instead of
/// silently claiming coverage: `dev_brief` refuses at its dependency-parallel
/// layer, while `ship_release_combined` — its `max … visits` bound lowered by
/// the fan-out parity landing — now surfaces the `on failure` refusal that
/// waited behind it.
#[test]
fn combined_loop_fixtures_advance_to_the_next_refusal() -> Result<(), Box<dyn std::error::Error>> {
    let dev_brief = manifest_dir().join("tests/fixtures/rev2/flagship/valid/dev_brief.awl");
    match lower_fixture(&dev_brief)? {
        Err(LowerError::Unsupported { shape, .. }) => {
            assert_eq!(shape, "parallel region", "dev_brief refusal drifted");
        }
        other => return Err(format!("dev_brief must still refuse: {other:?}").into()),
    }
    let ship =
        manifest_dir().join("tests/fixtures/rev2/loop-outcomes/valid/ship_release_combined.awl");
    match lower_fixture(&ship)? {
        Err(LowerError::Unsupported { shape, .. }) => {
            assert!(
                shape.contains("on failure"),
                "ship_release refusal drifted: {shape}"
            );
        }
        other => {
            return Err(format!("ship_release must refuse at `on failure`: {other:?}").into());
        }
    }
    Ok(())
}

/// `guard_optional_wait` was the wait-deferral pin; timeout waits now lower,
/// so it flips to a lowering + verify coverage pin (the same promotion as
/// `post_join_combinator_now_lowers`).
#[test]
fn guard_optional_wait_now_lowers() -> Result<(), Box<dyn std::error::Error>> {
    let path =
        manifest_dir().join("tests/fixtures/rev2/loop-outcomes/valid/guard_optional_wait.awl");
    let module = lower_fixture(&path)??;
    super::verify(&module)?;
    Ok(())
}

/// The historical post-join combinator refusal, kept as a coverage pin: a
/// supported collection join followed by exactly one combinator now lowers.
#[test]
fn post_join_combinator_now_lowers() -> Result<(), Box<dyn std::error::Error>> {
    let source = "\
//! Focused post-join combinator coverage pin.
workflow post_join_combinator
  input docs: [Doc]
  outcome done: type Done, route success

type Doc     { title: String }
type Verdict { accepted: Bool }
type Done    { count: Int }

worker review
  action check_doc(doc: Doc) -> Verdict

step check_all
  fork doc in docs
    check_doc(doc: doc)
  join -> verdicts

  verdicts |> count -> total
  route done(count: total)
";
    let module = lower_source(source)??;
    super::verify(&module)?;
    Ok(())
}

/// A document whose ONLY child-spawn forms are STATEMENT-LEVEL (an awaited
/// child call, or a fire-and-forget `spawn`) must still plan the T-WIT
/// witness — `needs_child_witness` covers `Statement::Call`/`Statement::Spawn`,
/// not just collection forks and pipe stages. The salvage defect was a
/// `child spawn has no planned witness` Planning error at lower time.
#[test]
fn statement_level_child_forms_plan_the_witness() -> Result<(), Box<dyn std::error::Error>> {
    for fixture in [
        "tests/fixtures/rev2/declarations/valid/child_call_awaited.awl",
        "tests/fixtures/rev2/declarations/valid/spawn_detached.awl",
    ] {
        let module = lower_fixture(&manifest_dir().join(fixture))??;
        super::verify(&module)?;
        let printed = format!("{module:?}");
        assert!(
            printed.contains("ChildWitness"),
            "{fixture} must carry the planned T-WIT function"
        );
    }
    Ok(())
}

/// `sort` over a non-comparable key is the reference emitter's hard gate; the
/// checker does not constrain key comparability, so a check-clean document
/// must refuse here with the reference class. Bool keys refuse too: the
/// shipped `gleam_stdlib` exports no `gleam@bool:compare/2`, so a Bool key
/// could only ever be a Gleam compile error (reference path) or an `undef`
/// crash (direct path).
#[test]
fn sort_non_comparable_key_refuses_with_the_reference_class()
-> Result<(), Box<dyn std::error::Error>> {
    for (key_type, field) in [("Inner", "inner"), ("Bool", "flag")] {
        let source = format!(
            "\
//! Focused non-comparable sort-key refusal.
workflow sort_non_comparable
  input docs: [Doc]
  outcome done: type Done, route success

type Inner {{ note: String }}
type Doc   {{ inner: Inner, flag: Bool }}
type Done  {{ total: Int }}

step order
  docs |> sort(.{field}) |> count -> total
  route done(total: total)
"
        );
        match lower_source(&source)? {
            Err(LowerError::Unsupported { shape, .. }) => {
                assert_eq!(
                    shape, "`sort` over a non-comparable key (needs Int, Float, String)",
                    "sort key type {key_type} must refuse"
                );
            }
            other => {
                return Err(format!(
                    "expected the non-comparable sort refusal for {key_type}, got {other:?}"
                )
                .into());
            }
        }
    }
    Ok(())
}

/// An ordinary action call whose argument indexes (`docs[0]`) now lowers
/// through `aion@awl@runtime:index/3`. The span-anchoring intent of the old
/// refusal pin survives as the byte-identical out-of-range message literal
/// (line 13, column 18 — the exact `docs[0]` slice), which must appear among
/// the module literals alongside the `RtIndex` runtime call.
/// The parallel-fork indexing-prelude class remains separately pinned in
/// `fork_tests::stopgap_refusals_match_the_reference_classes`.
#[test]
fn ordinary_call_index_lowers_with_the_anchored_runtime_message()
-> Result<(), Box<dyn std::error::Error>> {
    let source = "\
//! Focused ordinary-call index coverage pin.
workflow ordinary_index
  input docs: [Doc]
  outcome done: type Done, route success

type Doc  { title: String }
type Done { title: String }

worker review
  action check_doc(doc: Doc) -> Done

step check_one
  check_doc(doc: docs[0]) -> checked
  route done(title: checked.title)
";
    let module = lower_source(source)??;
    super::verify(&module)?;
    let printed = format!("{module:?}");
    assert!(
        printed.contains("RtIndex"),
        "index lowering must ride aion@awl@runtime:index/3"
    );
    let message: Vec<u8> = b"index 0 out of range at line 13, column 18".to_vec();
    let anchored = module
        .literals
        .iter()
        .any(|literal| matches!(literal, super::MirLiteral::Binary(bytes) if *bytes == message));
    assert!(
        anchored,
        "the out-of-range message must stay anchored to line 13, column 18"
    );
    Ok(())
}
