//! BC-2b-4 fork parity pins: exact generated-code fragments for the four
//! fork semantics over the new fixtures — two-sided with the MIR pins in
//! `src/mir/fork_tests.rs`, which anchor the SAME fragments in printed MIR.

use std::error::Error;

use super::emitted_fixture;

/// Parallel action collection fork (`doc_certification`'s shape): one
/// `workflow.map` whose branch closes over the free name, collapsed through
/// `map_activity_error` — the exact reference line.
#[test]
fn parallel_action_fork_is_one_map_with_error_collapse() -> Result<(), Box<dyn Error>> {
    let generated = emitted_fixture("dag-fork/valid/fork_action_fanout.awl")?;
    assert!(
        generated.contains(
            "use reports <- result.try(workflow.map(halves, fn(half) { \
             review_half_activity(half, revision) |> activity.task_queue(\"review\") }) \
             |> awl_error.map_activity_error)"
        ),
        "parallel fork must ride one workflow.map with the captured free name: {generated}"
    );
    assert!(
        !generated.contains("workflow.spawn"),
        "an action fork must never spawn child workflows: {generated}"
    );
    Ok(())
}

/// Sequential collection fork: the fold starts from the EMPTY accumulator,
/// runs each item durably in input order, prepends, and the join reverses —
/// input-ordered results.
#[test]
fn sequential_fork_folds_from_empty_and_reverses() -> Result<(), Box<dyn Error>> {
    let generated = emitted_fixture("dag-fork/valid/fork_sequential_route.awl")?;
    let fold = generated
        .find("use awl_folded <- result.try(list.try_fold(migrations, [], fn(awl_acc, migration) {")
        .ok_or("sequential fork must fold from the empty accumulator")?;
    let run = generated
        .find(
            "use awl_item <- result.try(apply_one_activity(migration) |> \
             activity.task_queue(\"db\") |> workflow.run |> awl_error.map_activity_error)",
        )
        .ok_or("each item must run durably with per-item error collapse")?;
    let prepend = generated
        .find("Ok([awl_item, ..awl_acc])")
        .ok_or("fold must prepend each item")?;
    let reverse = generated
        .find("let receipts = list.reverse(awl_folded)")
        .ok_or("the join must reverse to input order")?;
    assert!(
        fold < run && run < prepend && prepend < reverse,
        "fold -> run -> prepend -> reverse order diverged: {generated}"
    );
    Ok(())
}

/// Homogeneous named fork: ONE typed `workflow.all` carrying every branch's
/// activity value in source order, destructured in source order — never the
/// raw twins.
#[test]
fn homogeneous_named_fork_is_one_typed_all() -> Result<(), Box<dyn Error>> {
    let generated = emitted_fixture("dag-fork/valid/fork_named_homogeneous.awl")?;
    assert!(
        generated.contains(
            "use awl_branches <- result.try(workflow.all([\
             probe_region_activity(primary) |> activity.task_queue(\"probe\"), \
             probe_region_activity(secondary) |> activity.task_queue(\"probe\")]) \
             |> awl_error.map_activity_error)"
        ),
        "homogeneous branches must share one typed workflow.all in source order: {generated}"
    );
    assert!(
        generated.contains("let assert [first_probe, second_probe] = awl_branches"),
        "the join must destructure in source order: {generated}"
    );
    assert!(
        !generated.contains("_activity_raw"),
        "homogeneous branches must ride the typed wrappers: {generated}"
    );
    Ok(())
}
