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

/// The two combined fixtures advance to their next honest refusal instead of
/// silently claiming coverage: `dev_brief` refuses at its dependency-parallel
/// layer, while `ship_release_combined` refuses at `wait`.
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
            assert_eq!(
                shape, "wait",
                "ship_release refusal must advance past the loop"
            );
        }
        other => return Err(format!("ship_release must refuse at wait: {other:?}").into()),
    }
    Ok(())
}

#[test]
fn deferred_fixture_errors_cleanly() -> Result<(), Box<dyn std::error::Error>> {
    let path =
        manifest_dir().join("tests/fixtures/rev2/loop-outcomes/valid/guard_optional_wait.awl");
    let result = lower_fixture(&path)?;
    assert!(matches!(
        result,
        Err(LowerError::Unsupported { .. } | LowerError::Planning { .. })
    ));
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

/// `sort` over a non-comparable key is the reference emitter's hard gate; the
/// checker does not constrain key comparability, so a check-clean document
/// must refuse here with the reference class.
#[test]
fn sort_non_comparable_key_refuses_with_the_reference_class()
-> Result<(), Box<dyn std::error::Error>> {
    let source = "\
//! Focused non-comparable sort-key refusal.
workflow sort_non_comparable
  input docs: [Doc]
  outcome done: type Done, route success

type Inner { note: String }
type Doc   { inner: Inner }
type Done  { total: Int }

step order
  docs |> sort(.inner) |> count -> total
  route done(total: total)
";
    match lower_source(source)? {
        Err(LowerError::Unsupported { shape, .. }) => {
            assert_eq!(
                shape,
                "`sort` over a non-comparable key (needs Int, Float, String, Bool)"
            );
        }
        other => {
            return Err(format!("expected the non-comparable sort refusal, got {other:?}").into());
        }
    }
    Ok(())
}

/// An otherwise-supported ordinary action call whose only deferred expression
/// is `docs[0]` must refuse at `lower/expr.rs`, anchored to that exact slice.
/// The parallel-fork indexing-prelude class remains separately pinned in
/// `fork_tests::stopgap_refusals_match_the_reference_classes`.
#[test]
fn ordinary_call_index_refuses_as_expression_at_the_index_span()
-> Result<(), Box<dyn std::error::Error>> {
    let source = "\
//! Focused ordinary-call index refusal.
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
    match lower_source(source)? {
        Err(LowerError::Unsupported { shape, span }) => {
            assert_eq!(shape, "expression");
            assert_eq!(span.line, 13, "index refusal moved off the call argument");
            assert_eq!(span.column, 18, "index refusal column drifted");
            assert_eq!(
                source.get(span.start..span.end),
                Some("docs[0]"),
                "refusal must anchor the complete offending index expression"
            );
        }
        other => {
            return Err(format!("expected focused index-expression refusal, got {other:?}").into());
        }
    }
    Ok(())
}
