//! Child collection-fork semantic pins, two-sided with the reference emitter.

use std::fs;
use std::path::PathBuf;

use super::{lower, print_mir, verify};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_artifacts(relative: &str) -> Result<(String, String), Box<dyn std::error::Error>> {
    let path = manifest_dir()
        .join("tests/fixtures/rev2/dag-fork/valid")
        .join(relative);
    let source = fs::read_to_string(&path)?;
    let document = crate::parse(&source)?;
    let module = lower(&document, path.parent())?;
    verify(&module)?;
    Ok((print_mir(&module), crate::emit(&document)?))
}

fn function<'a>(text: &'a str, heading: &str) -> Result<&'a str, Box<dyn std::error::Error>> {
    text.split(heading)
        .nth(1)
        .and_then(|tail| tail.split("== fn ").next())
        .ok_or_else(|| format!("missing MIR function `{heading}`").into())
}

/// Parallelism and result order: the host completes a spawn fold over source
/// items, then awaits the reversed handle list while prepending each result.
/// Thus no await is reachable until every child has spawned, and the double
/// reversal restores collection order rather than completion order.
#[test]
fn parallel_child_fork_is_spawn_all_then_ordered_await_with_emitter_parity()
-> Result<(), Box<dyn std::error::Error>> {
    let (mir, emitted) = fixture_artifacts("child_collection_fork.awl")?;
    let host = function(&mir, "== fn step_fan_out/2")?;
    let spawn_closure = host
        .find("make_closure fan_out_fork_0 captures=[v0]")
        .ok_or("missing child spawn folder closure")?;
    let spawn_fold = host
        .find("call_rt gleam@list:try_fold/3(v1, nil, v2)")
        .ok_or("missing child spawn fold")?;
    let handles_bound = host
        .find("v4 = try_bind v3")
        .ok_or("missing handle-list bind")?;
    let await_closure = host
        .find("make_closure fan_out_fork_1 captures=[]")
        .ok_or("missing child await folder closure")?;
    let await_fold = host
        .find("call_rt gleam@list:try_fold/3(v4, nil, v5)")
        .ok_or("await fold must consume the reversed handle list")?;
    let results_bound = host
        .find("v7 = try_bind v6")
        .ok_or("missing result-list bind")?;
    assert!(
        spawn_closure < spawn_fold
            && spawn_fold < handles_bound
            && handles_bound < await_closure
            && await_closure < await_fold
            && await_fold < results_bound,
        "spawn fold must complete before the ordered await fold:\n{host}"
    );
    assert!(
        host.contains("tail_local step_report(v7)"),
        "the ordered join result must feed the collection-order report:\n{host}"
    );

    let spawn_body = function(&mir, "== fn fan_out_fork_0/3")?;
    let await_body = function(&mir, "== fn fan_out_fork_1/2")?;
    assert!(spawn_body.contains("aion@workflow:spawn/6"));
    assert!(!spawn_body.contains("aion@child:await/1"));
    assert!(await_body.contains("aion@child:await/1(v1)"));
    assert!(!await_body.contains("aion@workflow:spawn/6"));
    assert!(
        await_body.contains("v5 = cons v4 v0"),
        "await results must prepend while traversing reversed handles:\n{await_body}"
    );

    let emitter_spawn_fold = emitted
        .find("use awl_handles_reversed <- result.try(list.try_fold")
        .ok_or("reference emitter lost its spawn fold")?;
    let emitter_spawn = emitted
        .find("workflow.spawn(\"sit_one\"")
        .ok_or("reference emitter lost string-name spawn")?;
    let emitter_await_fold = emitted
        .find("use awl_children <- result.try(list.try_fold(awl_handles_reversed")
        .ok_or("reference emitter lost its ordered await fold")?;
    let emitter_await = emitted
        .find("child.await(awl_handle)")
        .ok_or("reference emitter lost child await")?;
    assert!(
        emitter_spawn_fold < emitter_spawn
            && emitter_spawn < emitter_await_fold
            && emitter_await_fold < emitter_await,
        "MIR and reference emitter must share spawn-all/ordered-await structure:\n{emitted}"
    );
    assert!(emitted.contains("let rows = awl_children"));
    Ok(())
}

/// Child identity and typed decode: each item invokes real string-name
/// `workflow.spawn`, carries the fixed witness, and passes the parent-side
/// `SittingRow` outcome-envelope codec in the spawn ABI's output slot.
#[test]
fn child_spawn_keeps_identity_and_outcome_envelope_codec() -> Result<(), Box<dyn std::error::Error>>
{
    let (mir, emitted) = fixture_artifacts("child_collection_fork.awl")?;
    let spawn = function(&mir, "== fn fan_out_fork_0/3")?;
    assert!(
        mir.contains("\"sit_one\"")
            && spawn.contains("aion@workflow:spawn/6(lit#16, v4, v3, v5, v6, v7)"),
        "every item must use the declared child registration name:\n{spawn}"
    );
    assert!(
        spawn.contains("v4 = make_closure awl$child_witness captures=[]"),
        "string-name child spawn must carry the fixed witness:\n{spawn}"
    );
    assert!(
        spawn.contains(
            "v3 = json_obj {\"spec\": v1|>sitting_spec_to_json, \
                        \"pack_revision\": v2|>awlc.string_to_json}"
        ),
        "child input must encode each declared parameter through its codec:\n{spawn}"
    );
    assert!(
        spawn.contains("v6 = call_local awl_child_output_sitting_row_codec()")
            && spawn.contains("spawn/6(lit#16, v4, v3, v5, v6, v7)"),
        "the spawn output slot must carry the strict parent-side envelope codec:\n{spawn}"
    );
    assert!(mir.contains("== durable families == children"));
    assert!(!mir.contains("aion@workflow:map/2 [activities]"));

    assert!(emitted.contains("workflow.spawn(\"sit_one\""));
    assert!(emitted.contains("awl_child_output_sitting_row_codec()"));
    assert!(emitted.contains("json.object([#(\"spec\", sitting_spec_to_json(spec))"));
    assert!(emitted.contains("use _outcome <- decode.field(\"outcome\", decode.string)"));
    assert!(emitted.contains("use payload <- decode.field(\"payload\", sitting_row_decoder())"));
    assert!(!emitted.contains("sit_one.execute"));
    Ok(())
}

/// Sequential parity is two-sided: both backends run one fold whose body uses
/// `spawn_and_wait`, bind the reversed accumulator, then reverse exactly once;
/// MIR routes that reverse result onward and the emitter binds it as `rows`.
#[test]
fn sequential_child_fork_matches_reference_fold_order() -> Result<(), Box<dyn std::error::Error>> {
    let (mir, emitted) = fixture_artifacts("child_collection_fork_sequential.awl")?;
    let host = function(&mir, "== fn step_run_all/1")?;
    assert_eq!(
        host.matches("gleam@list:try_fold/3").count(),
        1,
        "sequential MIR must contain exactly one fold:\n{host}"
    );
    assert_eq!(
        host.matches("gleam@list:reverse/1").count(),
        1,
        "sequential MIR must contain exactly one reverse:\n{host}"
    );
    let fold = host
        .find("v2 = call_rt gleam@list:try_fold/3(v0, nil, v1)")
        .ok_or("sequential MIR lost its fold")?;
    let bound = host
        .find("v3 = try_bind v2")
        .ok_or("sequential MIR lost its fold TryBind")?;
    let reverse = host
        .find("v4 = call_rt gleam@list:reverse/1(v3)")
        .ok_or("sequential MIR lost its accumulator reverse")?;
    let routed = host
        .find("tail_local step_report(v4)")
        .ok_or("sequential MIR must route the reverse result onward")?;
    assert!(
        fold < bound && bound < reverse && reverse < routed,
        "fold -> TryBind -> reverse -> route order diverged:\n{host}"
    );
    let body = function(&mir, "== fn run_all_fork_0/2")?;
    assert!(body.contains("aion@workflow:spawn_and_wait/6"));
    assert!(body.contains("aion@awl@error:map_child_error/1"));
    assert!(!body.contains("aion@workflow:spawn/6"));
    assert!(!body.contains("aion@child:await/1"));

    assert_eq!(emitted.matches("list.try_fold(").count(), 1);
    assert_eq!(emitted.matches("list.reverse(").count(), 1);
    let emitter_fold = emitted
        .find("use awl_children_reversed <- result.try(list.try_fold")
        .ok_or("reference emitter lost its sequential child fold")?;
    let emitter_wait = emitted
        .find("workflow.spawn_and_wait(\"run_one\"")
        .ok_or("reference emitter lost sequential string-name spawn_and_wait")?;
    let emitter_reverse = emitted
        .find("let rows = list.reverse(awl_children_reversed)")
        .ok_or("reference emitter must bind the single reverse as `rows`")?;
    assert!(
        emitter_fold < emitter_wait && emitter_wait < emitter_reverse,
        "reference fold -> spawn_and_wait -> reverse order diverged:\n{emitted}"
    );
    Ok(())
}
