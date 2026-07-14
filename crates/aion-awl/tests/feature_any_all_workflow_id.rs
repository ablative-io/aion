//! Two-backend structural parity for collection predicates and `workflow.id`.

use std::fs;
use std::path::{Path, PathBuf};

use aion_awl::mir::{lower, print_mir, select};
use aion_awl::{check_in, emit_in, parse};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rev2/step-bodies/valid")
        .join(name)
}

fn backends(name: &str) -> Result<(String, String), Box<dyn std::error::Error>> {
    let path = fixture(name);
    let source = fs::read_to_string(&path)?;
    let document = parse(&source)?;
    let root = path
        .parent()
        .ok_or_else(|| format!("fixture has no parent: {}", path.display()))?;
    let errors = check_in(&document, root);
    if !errors.is_empty() {
        return Err(format!("fixture did not check cleanly: {errors:?}").into());
    }
    let gleam = emit_in(&document, root)?;
    let module = lower(&document, Some(root))?;
    let _ = select(&module)?;
    Ok((gleam, print_mir(&module)))
}

#[test]
fn nested_any_all_has_reference_direct_structural_parity() -> TestResult {
    let (gleam, mir) = backends("collection_predicates.awl")?;
    assert!(gleam.matches("list.any(").count() >= 4);
    assert!(gleam.matches("list.all(").count() >= 3);
    assert!(gleam.contains(".severity == \"blocker\""));
    assert!(mir.matches("call_rt gleam@list:any/2").count() >= 4);
    assert!(mir.matches("call_rt gleam@list:all/2").count() >= 3);
    assert!(mir.matches("make_closure").count() >= 7);
    Ok(())
}

#[test]
fn workflow_id_has_reference_direct_structural_parity() -> TestResult {
    let (gleam, mir) = backends("workflow_id.awl")?;
    assert_eq!(gleam.matches("workflow.id()").count(), 2);
    assert!(gleam.contains("awl_error.map_engine_error"));
    assert_eq!(mir.matches("call_rt aion@workflow:id/0").count(), 2);
    assert_eq!(
        mir.matches("call_rt aion@awl@error:map_engine_error/1")
            .count(),
        2
    );
    assert!(mir.contains("concat"));
    Ok(())
}

#[test]
fn fallible_predicates_have_result_callbacks_and_outer_binds() -> TestResult {
    let (gleam, mir) = backends("fallible_collection_predicates.awl")?;
    assert_eq!(gleam.matches("list.try_fold(").count(), 2);
    assert_eq!(gleam.matches("workflow.id()").count(), 2);
    assert!(gleam.contains("False, fn(awl_acc"));
    assert!(gleam.contains("True, fn(awl_acc"));
    assert!(gleam.contains("True -> Ok(True)"));
    assert!(gleam.contains("False -> Ok(False)"));
    assert_eq!(mir.matches("call_rt gleam@list:try_fold/3").count(), 2);
    assert_eq!(mir.matches("if is_true v0:").count(), 2);
    assert_eq!(
        mir.matches("-> Result(Bool, AwlError)").count(),
        2,
        "fallible lifted callbacks must not claim a Bool return contract"
    );
    assert!(mir.contains("try_fold/3(v0, 'false'"));
    assert!(mir.contains("try_fold/3(v0, 'true'"));
    Ok(())
}
