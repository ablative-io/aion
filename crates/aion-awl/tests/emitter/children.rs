//! Child workflow and detached-spawn emitter regressions.

use std::error::Error;

use aion_awl::{emit, parse};

use super::{emitted_archived_exam, emitted_fixture};

/// Child calls keep the string-name spawn discipline: registration-name
/// spawn, JSON-object input, a witness fn the SDK never calls, and no
/// phantom child module references.
#[test]
fn child_call_lowers_to_string_name_spawn() -> Result<(), Box<dyn Error>> {
    let generated = emitted_fixture("declarations/valid/child_call_awaited.awl")?;
    assert!(generated.contains("workflow.spawn_and_wait(\"score_essay\""));
    assert!(
        generated.contains("json.object([#(\"essay\", awlc.string_to_json(essay))])"),
        "named child args must encode as one JSON object: {generated}"
    );
    assert!(generated.contains(
        "fn(_: json.Json) { Error(awl_error.AwlChildFailed(\"child workflow body runs in its own \
         execution\")) }"
    ));
    assert!(!generated.contains("score_essay.execute"));
    assert!(!generated.contains("score_essay.input_codec"));
    Ok(())
}

/// A collection fork over a child starts every child before awaiting results,
/// preserving input order while providing true per-item child-run fan-out.
#[test]
fn child_collection_fork_spawns_all_then_awaits_all() -> Result<(), Box<dyn Error>> {
    let generated = emitted_fixture("dag-fork/valid/child_collection_fork.awl")?;
    let spawn = generated
        .find("workflow.spawn(\"sit_one\"")
        .ok_or("child fork must emit a child spawn")?;
    let await_child = generated
        .find("child.await(awl_handle)")
        .ok_or("child fork must await spawned handles")?;
    assert!(
        spawn < await_child,
        "all-child spawn fold must precede await fold"
    );
    assert!(generated.contains("import aion/child"));
    assert!(generated.contains("let rows = awl_children"));

    let archived_exam = emitted_archived_exam()?;
    assert!(archived_exam.contains("workflow.spawn(\"sit_one\""));
    assert!(archived_exam.contains("let rows = awl_children"));
    Ok(())
}

/// Unsupported child calls in named forks report the construct that is not
/// lowerable instead of falsely claiming the declared child is not an action.
#[test]
fn named_fork_child_refusal_is_honest() -> Result<(), Box<dyn Error>> {
    let source = "\
//! Honest refusal regression.
workflow honest_refusal
  input value: String
  outcome done: type String, route success

child inspect(value: String) -> String

step inspect
  fork
    inspect(value: value) -> inspected
  join
  inspected |> route done
";
    let error = emit(&parse(source)?).err().ok_or("emit must refuse")?;
    assert_eq!(
        error.message,
        "child calls are not yet lowerable inside named fork branches"
    );
    assert!(!error.message.contains("no declared action"));
    Ok(())
}

/// `spawn` is detached: the SDK spawn is used, the handle is discarded, and
/// the workflow continues.
#[test]
fn spawn_lowers_detached() -> Result<(), Box<dyn Error>> {
    let generated = emitted_fixture("declarations/valid/spawn_detached.awl")?;
    assert!(
        generated.contains("use _ <- result.try(workflow.spawn(\"audit_trail\""),
        "spawn must be detached and unbound: {generated}"
    );
    assert!(
        generated.contains("map_spawn_error"),
        "a failed detached spawn is a step failure: {generated}"
    );
    assert!(!generated.contains("spawn_and_wait(\"audit_trail\""));
    Ok(())
}
