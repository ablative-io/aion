//! BC-2b-4 fork-lowering pins: the four fork semantics asserted against the
//! printed MIR (two-sided with `tests/emitter/forks.rs`, which pins the SAME
//! fragments in the reference emitter's generated Gleam), plus the
//! emitter-parity refusal classes (everything the reference refuses, we
//! refuse — cleanly, at lower).

use std::fs;
use std::path::{Path, PathBuf};

use super::{LowerError, MirModule, lower, print_mir, verify};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lowered_fixture(relative: &str) -> Result<String, Box<dyn std::error::Error>> {
    let path = manifest_dir().join("tests/fixtures/rev2").join(relative);
    let source = fs::read_to_string(&path)?;
    let document = crate::parse(&source)?;
    let module = lower(&document, path.parent())?;
    verify(&module)?;
    Ok(print_mir(&module))
}

/// Parse (a hard test error on failure) and lower an inline pin source; the
/// inner `Result` is the lowering outcome under test.
fn lower_source(source: &str) -> Result<Result<MirModule, LowerError>, Box<dyn std::error::Error>> {
    let document = crate::parse(source)
        .map_err(|error| format!("inline pin source no longer parses: {error}"))?;
    Ok(lower(&document, Some(Path::new("."))))
}

/// R1+R2+R3+R4, parallel action collection fork (`doc_certification`'s
/// shape): captures close over the branch's free names BEFORE the single
/// `workflow.map` dispatch; failures collapse through `map_activity_error`;
/// the branch body returns the UNRUN configured activity value (never
/// `workflow.run` — the engine owns dispatch, ordering, and cancellation).
#[test]
fn parallel_action_fork_pins_map_fanout_semantics() -> Result<(), Box<dyn std::error::Error>> {
    let text = lowered_fixture("dag-fork/valid/fork_action_fanout.awl")?;

    let host = text
        .split("== fn step_fan_out/2")
        .nth(1)
        .and_then(|tail| tail.split("== fn ").next())
        .ok_or("missing fan_out step")?;
    let closure = host
        .find("make_closure fan_out_fork_0 captures=[v1]")
        .ok_or("map closure must capture the free name (deterministic order)")?;
    let map = host
        .find("call_rt aion@workflow:map/2(v0, v2)")
        .ok_or("fork must ride one workflow.map over the collection")?;
    let collapse = host
        .find("call_rt aion@awl@error:map_activity_error/1")
        .ok_or("map result must collapse through map_activity_error")?;
    let bind = host.find(" = try_bind ").ok_or("joined bind missing")?;
    assert!(
        closure < map && map < collapse && collapse < bind,
        "capture -> map -> error collapse -> bind order diverged:\n{host}"
    );

    let branch = text
        .split("== fn fan_out_fork_0/2 origin=fork(fan_out#0) ==")
        .nth(1)
        .and_then(|tail| tail.split("== fn ").next())
        .ok_or("missing fork branch body")?;
    assert!(
        branch.contains("-> Activity("),
        "branch body must return the activity VALUE type:\n{branch}"
    );
    assert!(
        branch.contains("call_rt aion@activity:task_queue/2"),
        "branch body must configure the task queue:\n{branch}"
    );
    assert!(
        !branch.contains("aion@workflow:run/1"),
        "a parallel branch must never run its own activity (the map does):\n{branch}"
    );
    Ok(())
}

/// Sequential collection fork: `list.try_fold` from the EMPTY initial
/// accumulator, per-item durable run + error collapse + prepend, then one
/// `list.reverse` so joined results are input-ordered (R3) — the reference's
/// exact fold shape.
#[test]
fn sequential_fork_pins_fold_initial_state_and_result_order()
-> Result<(), Box<dyn std::error::Error>> {
    let text = lowered_fixture("dag-fork/valid/fork_sequential_route.awl")?;

    let host = text
        .split("== fn step_apply_all/1")
        .nth(1)
        .and_then(|tail| tail.split("== fn ").next())
        .ok_or("missing apply_all step")?;
    assert!(
        host.contains("call_rt gleam@list:try_fold/3(v0, nil, v1)"),
        "the fold must start from the empty list:\n{host}"
    );
    assert!(
        host.contains("v4 = call_rt gleam@list:reverse/1(v3)"),
        "the joined list must reverse the fold accumulator:\n{host}"
    );
    assert!(
        host.contains("v5 = record(applied, [v4])"),
        "the route payload must consume the REVERSED (input-order) list:\n{host}"
    );

    let branch = text
        .split("== fn apply_all_fork_0/2 origin=fork(apply_all#0) ==")
        .nth(1)
        .and_then(|tail| tail.split("== fn ").next())
        .ok_or("missing fold body")?;
    let run = branch
        .find("call_rt aion@workflow:run/1")
        .ok_or("sequential branches run durably, one at a time")?;
    let collapse = branch
        .find("call_rt aion@awl@error:map_activity_error/1")
        .ok_or("per-item error collapse missing")?;
    let bind = branch.find(" = try_bind ").ok_or("item bind missing")?;
    let cons = branch
        .find(" = cons ")
        .ok_or("accumulator prepend missing")?;
    let ok = branch.find("record(ok, [").ok_or("Ok wrap missing")?;
    assert!(
        run < collapse && collapse < bind && bind < cons && cons < ok,
        "run -> collapse -> bind -> prepend -> Ok order diverged:\n{branch}"
    );
    Ok(())
}

/// Homogeneous named fork: source-order activity values in exactly ONE typed
/// `workflow.all` (never raw twins), destructured in source order.
#[test]
fn homogeneous_named_fork_pins_one_typed_all() -> Result<(), Box<dyn std::error::Error>> {
    let text = lowered_fixture("dag-fork/valid/fork_named_homogeneous.awl")?;
    assert!(
        !text.contains("_activity_raw"),
        "homogeneous branches ride the TYPED wrappers:\n{text}"
    );
    assert_eq!(
        text.matches("call_rt aion@workflow:all/1").count(),
        1,
        "every branch dispatches in one workflow.all"
    );
    let host = text
        .split("== fn step_gather/2")
        .nth(1)
        .and_then(|tail| tail.split("== fn ").next())
        .ok_or("missing gather step")?;
    let first = host
        .find("call_local probe_region_activity(v0)")
        .ok_or("first branch value missing")?;
    let second = host
        .find("call_local probe_region_activity(v1)")
        .ok_or("second branch value missing")?;
    let list = host.find("v6 = list([v3, v5])").ok_or("all-list missing")?;
    assert!(
        first < second && second < list,
        "branch values must build in source order before the all-call:\n{host}"
    );
    assert!(
        host.contains("assert_list [v10, v11] = v9"),
        "join must destructure in source order:\n{host}"
    );
    assert!(
        host.contains("record(compared, [v10, v11])"),
        "branch binds must keep source positions:\n{host}"
    );
    Ok(())
}

/// R5, heterogeneous named fork: every branch rides its raw wire-unified
/// twin in ONE `workflow.all`, and each bound position decodes with THAT
/// action's return codec and string action name.
#[test]
fn heterogeneous_named_fork_pins_raw_twins_and_positional_decode()
-> Result<(), Box<dyn std::error::Error>> {
    let text = lowered_fixture("dag-fork/valid/fork_named_branches.awl")?;
    assert!(
        text.contains("T-ACTRAW action=fetch_profile"),
        "raw twin shell for fetch_profile missing:\n{text}"
    );
    assert!(
        text.contains("T-ACTRAW action=fetch_history"),
        "raw twin shell for fetch_history missing:\n{text}"
    );
    let host = text
        .split("== fn step_gather/1")
        .nth(1)
        .and_then(|tail| tail.split("== fn ").next())
        .ok_or("missing gather step")?;
    let profile = host
        .find("call_local fetch_profile_activity_raw(v0)")
        .ok_or("profile branch must ride its raw twin")?;
    let history = host
        .find("call_local fetch_history_activity_raw(v0)")
        .ok_or("history branch must ride its raw twin")?;
    let all = host
        .find("call_rt aion@workflow:all/1")
        .ok_or("single all-dispatch missing")?;
    assert!(
        profile < history && history < all,
        "raw branch values must build in source order before the all-call:\n{host}"
    );
    assert!(
        host.contains("assert_list [v9, v10] = v8"),
        "join must destructure raw payloads by source position:\n{host}"
    );
    // Position 0 decodes with profile's return codec + its action name;
    // position 1 with history's — never the other way around.
    assert!(
        host.contains("v11 = call_local profile_codec()")
            && host.contains("call_rt aion@awl@codec:decoded/3(v11, v9, lit#2)"),
        "position 0 must decode with the profile return codec:\n{host}"
    );
    assert!(
        host.contains("v14 = call_local history_codec()")
            && host.contains("call_rt aion@awl@codec:decoded/3(v14, v10, lit#3)"),
        "position 1 must decode with the history return codec:\n{host}"
    );
    Ok(())
}

/// R1: the same AST call shape routes distinctly — an ACTION collection fork
/// rides in-workflow activity fan-out (`workflow.map`, the Activities
/// durable family), while a CHILD collection fork keeps refusing at lower
/// with a clean diagnostic (the child witness shell does not select yet).
#[test]
fn action_and_child_collection_forks_route_distinctly() -> Result<(), Box<dyn std::error::Error>> {
    let action = lowered_fixture("dag-fork/valid/fork_action_fanout.awl")?;
    assert!(
        action.contains("aion@workflow:map/2 [activities]"),
        "action fan-out must be the Activities durable family:\n{action}"
    );
    assert!(
        !action.contains("spawn"),
        "an action fork must never spawn child workflows:\n{action}"
    );

    let path = manifest_dir().join("tests/fixtures/rev2/dag-fork/valid/child_collection_fork.awl");
    let source = fs::read_to_string(&path)?;
    let document = crate::parse(&source)?;
    match lower(&document, path.parent()) {
        Err(LowerError::Unsupported { shape, .. }) => {
            assert_eq!(shape, "child collection fork", "child routing pin drifted");
        }
        other => return Err(format!("child fork must refuse cleanly: {other:?}").into()),
    }
    Ok(())
}

const REFUSAL_HEADER: &str = "\
//! Pin: fork stopgap refusals stay clean.
workflow fork_pin
  input docs: [Doc]
  outcome done: type Done, route success

type Doc  { title: String }
type Done { count: Int }

worker review
  action check_doc(doc: Doc) -> Done

";

/// Everything the reference emitter refuses, lowering refuses with a clean
/// `Unsupported` (the same diagnostic class): multi-statement collection
/// bodies, bound collection calls, parallel indexing preludes, named-child
/// branches, non-action named branches.
#[test]
fn stopgap_refusals_match_the_reference_classes() -> Result<(), Box<dyn std::error::Error>> {
    let cases: &[(&str, &str)] = &[
        (
            "step check_all
  fork doc in docs
    check_doc(doc: doc)
    check_doc(doc: doc)
  join -> results

  route done(count: 1)
",
            "a collection fork body beyond one unbound call",
        ),
        (
            "step check_all
  fork doc in docs
    check_doc(doc: doc) -> verdict
  join -> results

  route done(count: 1)
",
            "a collection fork body beyond one unbound call",
        ),
        (
            "step check_all
  fork doc in docs
    check_doc(doc: docs[0])
  join -> results

  route done(count: 1)
",
            "indexing inside a parallel fork branch",
        ),
        (
            "step check_all
  fork
    sleep 30s
  join

  route done(count: 1)
",
            "a named fork branch beyond an action call",
        ),
    ];
    for (body, expected) in cases {
        let source = format!("{REFUSAL_HEADER}{body}");
        match lower_source(&source)? {
            Err(LowerError::Unsupported { shape, .. }) => {
                assert_eq!(&shape, expected, "refusal class drifted for:\n{body}");
            }
            other => {
                return Err(format!("expected `{expected}` refusal, got {other:?}").into());
            }
        }
    }
    Ok(())
}

/// The named-child branch keeps the reference's exact stopgap class
/// (`tests/emitter/children.rs` pins the emitter side).
#[test]
fn named_child_branch_refuses_cleanly() -> Result<(), Box<dyn std::error::Error>> {
    let source = "\
//! Pin: named-fork child branches refuse cleanly.
workflow fork_pin_child
  input essay: String
  outcome done: type Done, route success

type Done  { count: Int }
type Score { value: Int }

worker review
  action check_essay(essay: String) -> Done

child score_essay(essay: String) -> Score

step gather
  fork
    check_essay(essay: essay) -> a
    score_essay(essay: essay) -> b
  join

  route done(count: 1)
";
    match lower_source(source)? {
        Err(LowerError::Unsupported { shape, .. }) => {
            assert_eq!(shape, "child calls inside named fork branches");
        }
        other => return Err(format!("expected the named-child refusal, got {other:?}").into()),
    }
    Ok(())
}
