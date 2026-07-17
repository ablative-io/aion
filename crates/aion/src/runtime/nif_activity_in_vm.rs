//! In-VM tier activity dispatch NIF — `aion_flow_ffi:dispatch_activity_in_vm/4`.
//!
//! The arity-4 wire is the arity-3 remote wire plus the runner thunk: the SDK
//! composes input capture, the runner, and the output codec into a zero-arity
//! closure, and this NIF spawns it as a LINKED child process of the workflow
//! (crash isolation — a runner panic kills the child, never the workflow
//! process). Everything recorded is byte-identical to a remote dispatch: the
//! same ordinal/correlation allocation, the same `resolve_command`, the same
//! `ActivityScheduled`/`ActivityStarted` shape with the resolved task queue,
//! node, and attempt stamped (NO event-schema change; the tier is deliberately
//! not recorded — like the dispatcher choice, a tier change between deploys is
//! a routing change, not workflow-visible nondeterminism). The await path is
//! untouched: the watcher delivers the child's outcome into the same
//! correlation/ordinal-keyed two-phase maps the remote completion task uses,
//! so runs-once / recorded / replay-returns-recording holds by construction,
//! and query pumping, `with_timeout` scope expiry, and the stale-snapshot
//! determinism discipline all apply unchanged.
//!
//! Crash recovery needs no new machinery: a node death after
//! `Scheduled`/`Started` with no terminal replays to `ResumeLive` (the cursor
//! walk exhausts, recording a fresh reopen `ActivityScheduled` for the same
//! ordinal), and replay re-executes workflow code — which re-supplies the
//! thunk — so the re-dispatch simply re-spawns the child: at-least-once, the
//! same contract as remote.
//!
//! Scheduling note: the child is a preemptively-scheduled bytecode process, so
//! pure-Gleam runners cannot starve the schedulers; only a blocking NIF called
//! INSIDE a runner can occupy a scheduler thread (beamr's dirty pool is still
//! scaffolded — `spawn_link_dirty` aliases `spawn_link`). Each in-flight in-VM
//! activity also parks one Tokio blocking-pool thread in its exit watcher
//! (`run_until_exit`); wide fan-outs of slow in-VM activities are bounded by
//! that pool, exactly like slow synchronous remote dispatchers.

use std::sync::Arc;

use crate::durability::{Command, CorrelationKey, ResolveOutcome};
use crate::runtime::nif_activity::{
    ScheduledActivity, context_error_term, correlation_id, decode_string_arg, error_result_term,
    json_payload, ok_result_term, record_started, runtime_context,
};
use crate::runtime::nif_activity_dispatch::FIRST_DELIVERY_ATTEMPT;
use crate::runtime::nif_context::NifContext;
use aion_core::ActivityId;
use beamr::native::ProcessContext;
use beamr::term::Term;
use beamr::term::boxed::Closure;

/// NIF backing `aion_flow_ffi:dispatch_activity_in_vm/4`.
pub(super) fn dispatch_activity_in_vm_impl(
    args: &[Term],
    ctx: &mut ProcessContext,
) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    // Decode (and validate the thunk's shape) BEFORE any ordinal allocation or
    // resolve: a malformed wire records nothing.
    let (name, input, config) = match decode_in_vm_args(args) {
        Ok(parts) => parts,
        Err(reason) => return Ok(error_result_term(ctx, &reason).unwrap_or(Term::NIL)),
    };
    let thunk = args[3];
    let Some(pid) = ctx.pid() else {
        return Ok(
            error_result_term(ctx, "dispatch_activity_in_vm: missing calling process pid")
                .unwrap_or(Term::NIL),
        );
    };
    let state = match super::nif_state::engine_nif_state(ctx) {
        Ok(state) => state,
        Err(error) => return Ok(error_result_term(ctx, &error).unwrap_or(Term::NIL)),
    };
    // dispatch_activity_in_vm records `ActivityScheduled`; a query handler
    // must stay read-only.
    if let Err(error) =
        super::nif_query_pump::ensure_not_servicing_query(&state, pid, "dispatch_activity_in_vm")
    {
        return Ok(error_result_term(ctx, &error).unwrap_or(Term::NIL));
    }
    let runtime = match runtime_context(&state) {
        Ok(runtime) => runtime,
        Err(error) => return Ok(context_error_term(ctx, &error)),
    };
    let context = match NifContext::new(
        pid,
        runtime.registry.as_ref(),
        runtime.tokio_handle.clone(),
        runtime.runtime.signal_delivery(),
    ) {
        Ok(context) => context,
        Err(error) => return Ok(context_error_term(ctx, &error)),
    };
    dispatch_in_vm_with_context(
        ctx,
        context,
        &runtime.runtime,
        &runtime.tokio_handle,
        (name, input, config),
        thunk,
    )
}

/// Decode the three string arguments and validate the fourth is a zero-arity
/// closure — the ONLY spawnable thunk shape. Rejection happens before anything
/// is recorded, so a defective wire cannot leave a pending `Scheduled`.
fn decode_in_vm_args(args: &[Term]) -> Result<(String, String, String), String> {
    if args.len() != 4 {
        return Err(format!(
            "dispatch_activity_in_vm: expected 4 arguments, got {}",
            args.len()
        ));
    }
    let name = decode_string_arg(args[0])
        .map_err(|error| format!("dispatch_activity_in_vm name: {error}"))?;
    let input = decode_string_arg(args[1])
        .map_err(|error| format!("dispatch_activity_in_vm input: {error}"))?;
    let config = decode_string_arg(args[2])
        .map_err(|error| format!("dispatch_activity_in_vm config: {error}"))?;
    let Some(thunk) = Closure::new(args[3]) else {
        return Err("dispatch_activity_in_vm: thunk argument is not a closure".to_owned());
    };
    if thunk.arity() != 0 {
        return Err(format!(
            "dispatch_activity_in_vm: thunk arity is {}, expected 0",
            thunk.arity()
        ));
    }
    Ok((name, input, config))
}

/// The recorded-resolution branch shared with the remote wire, then the in-VM
/// live branch: record, spawn the linked thunk child, arm the exit watcher.
///
/// GC-ordering contract: the thunk closure lives on the CALLER's heap, and
/// result-term allocation through `ctx` may collect (moving it), so the child
/// spawn — whose environment deep copy is the last read of the closure —
/// happens before the `ok_result_term` allocation. Every allocation before it
/// is on an error path that returns without spawning.
fn dispatch_in_vm_with_context(
    ctx: &mut ProcessContext,
    mut context: NifContext,
    runtime: &Arc<crate::RuntimeHandle>,
    tokio_handle: &tokio::runtime::Handle,
    (name, input, config): (String, String, String),
    thunk: Term,
) -> Result<Term, Term> {
    let input_payload = json_payload(ctx, &input, "dispatch_activity_in_vm", "input")?;
    let ordinal = context.next_activity_ordinal();
    let key = CorrelationKey::Activity(ordinal);
    let activity_id = ActivityId::from_sequence_position(ordinal);
    let correlation = correlation_id(ordinal);
    match context
        .resolve_command(Command::RunActivity {
            key,
            activity_type: name.clone(),
            input: input_payload.clone(),
        })
        .map_err(|error| context_error_term(ctx, &error))?
    {
        ResolveOutcome::Recorded(_) => {
            Ok(ok_result_term(ctx, correlation.as_bytes()).unwrap_or(Term::NIL))
        }
        ResolveOutcome::ResumeLive => {
            // Stamp the SAME resolved task queue / node / attempt a remote
            // dispatch would, so in-VM history is shape-identical (an
            // operator can re-tier an activity without a history migration).
            let start_time_task_queue = context.start_time_task_queue();
            let task_queue =
                super::nif_activity::resolve_task_queue(&config, start_time_task_queue.as_deref());
            let node = super::nif_activity::resolve_node(&config);
            record_started(
                ctx,
                &context,
                activity_id,
                ScheduledActivity {
                    activity_type: name,
                    input: input_payload,
                    task_queue,
                    node,
                    attempt: FIRST_DELIVERY_ATTEMPT,
                },
            )?;
            match runtime.spawn_activity_closure(context.pid(), thunk) {
                Ok(child_pid) => {
                    spawn_in_vm_completion_watcher(
                        tokio_handle,
                        Arc::clone(runtime),
                        context.pid(),
                        child_pid,
                        correlation.clone(),
                    );
                }
                Err(error) => retain_spawn_failure(runtime, context.pid(), &correlation, &error),
            }
            Ok(ok_result_term(ctx, correlation.as_bytes()).unwrap_or(Term::NIL))
        }
    }
}

/// Retain the deterministic terminal for a child spawn that failed after the
/// activity was recorded as started. The workflow is executing this NIF, so a
/// marker refusal is expected; the next await reads the retained keyed error.
pub(super) fn retain_spawn_failure(
    runtime: &crate::RuntimeHandle,
    workflow_pid: u64,
    correlation: &str,
    error: &impl std::fmt::Display,
) {
    let reason = format!("terminal:in-vm activity child spawn failed: {error}");
    if let Err(delivery_error) =
        runtime.deliver_activity_failure_message(workflow_pid, correlation, reason)
    {
        // The failure entry is retained before the wake marker is attempted;
        // the await's first pass therefore settles without requiring the wake.
        tracing::debug!(
            %delivery_error,
            workflow_pid,
            correlation_id = correlation,
            "in-vm spawn-failure marker not queued; retained entry settles the await"
        );
    }
}

/// Arm the exit watcher for one in-VM activity child (the in-VM mirror of
/// `spawn_completion_task`): block until the linked child exits, decode its
/// outcome at the exit boundary, and deliver it on the SAME correlation the
/// await resolves — `take_runtime_completion` then records
/// `ActivityCompleted`/`ActivityFailed` exactly as for remote.
///
/// Runs on the Tokio blocking pool: `run_until_exit` parks its thread until
/// the child exits, one thread per in-flight in-VM activity. Delivery to a
/// workflow that died meanwhile fails with a warn and the retained entry is
/// drained by the workflow process monitor (D5).
fn spawn_in_vm_completion_watcher(
    tokio_handle: &tokio::runtime::Handle,
    runtime: Arc<crate::RuntimeHandle>,
    workflow_pid: u64,
    child_pid: u64,
    correlation_id: String,
) {
    tokio_handle.spawn_blocking(move || {
        let outcome = runtime.in_vm_child_outcome(child_pid);
        // The child has an exit tombstone now: drop it from the workflow's
        // teardown set so a later workflow exit does not re-kill a dead pid.
        runtime.deregister_in_vm_child(workflow_pid, child_pid);
        let delivered = match outcome {
            crate::runtime::handle::InVmChildOutcome::Completed(payload) => {
                runtime.deliver_activity_completion_message(workflow_pid, &correlation_id, payload)
            }
            crate::runtime::handle::InVmChildOutcome::Failed(reason) => {
                runtime.deliver_activity_failure_message(workflow_pid, &correlation_id, reason)
            }
        };
        if let Err(error) = delivered {
            tracing::warn!(
                %error,
                workflow_pid,
                child_pid,
                correlation_id,
                "in-vm activity outcome delivery failed"
            );
        }
    });
}
