//! Two-phase activity dispatch NIFs.

use std::sync::Arc;

use crate::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use crate::durability::{Command, CorrelationKey, ResolveOutcome};
use crate::runtime::nif_activity::{
    context_error_term, correlation_id, decode_string_arg, error_result_term, json_payload,
    labels_from_config, ok_result_term, record_started, runtime_context,
};
use crate::runtime::nif_context::NifContext;
use aion_core::ActivityId;
use beamr::native::ProcessContext;
use beamr::term::Term;

/// NIF backing `aion_flow_ffi:dispatch_activity/3`.
pub(super) fn dispatch_activity_impl(
    args: &[Term],
    ctx: &mut ProcessContext,
) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    let Ok((name, input, config)) = decode_dispatch_args(args) else {
        return Ok(error_result_term(
            ctx,
            &format!(
                "dispatch_activity: expected 3 arguments, got {}",
                args.len()
            ),
        )
        .unwrap_or(Term::NIL));
    };
    // Defense: an in-VM selection must cross the arity-4 wire that carries the
    // runner thunk. Refused BEFORE any ordinal allocation or resolve, so
    // nothing is recorded.
    if super::nif_activity::config_tier(&config).as_deref() == Some(super::nif_activity::IN_VM_TIER)
    {
        return Ok(error_result_term(
            ctx,
            "dispatch_activity: tier in_vm cannot cross the remote dispatch wire — \
             in-VM dispatch requires dispatch_activity_in_vm/4 carrying the runner thunk",
        )
        .unwrap_or(Term::NIL));
    }
    let Some(pid) = ctx.pid() else {
        return Ok(
            error_result_term(ctx, "dispatch_activity: missing calling process pid")
                .unwrap_or(Term::NIL),
        );
    };
    let state = match super::nif_state::engine_nif_state(ctx) {
        Ok(state) => state,
        Err(error) => return Ok(error_result_term(ctx, &error).unwrap_or(Term::NIL)),
    };
    // dispatch_activity records `ActivityScheduled`; a query handler must
    // stay read-only.
    if let Err(error) =
        super::nif_query_pump::ensure_not_servicing_query(&state, pid, "dispatch_activity")
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
    let dispatcher = state.activity_dispatcher();
    dispatch_activity_with_context(
        ctx,
        context,
        dispatcher,
        runtime.runtime,
        &runtime.tokio_handle,
        ActivityCall {
            name,
            input,
            config,
            attempt: FIRST_DELIVERY_ATTEMPT,
        },
    )
}

/// NIF backing `aion_flow_ffi:await_activity_result/1`.
pub(super) fn await_activity_result_impl(
    args: &[Term],
    ctx: &mut ProcessContext,
) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if args.len() != 1 {
        return Ok(error_result_term(
            ctx,
            &format!(
                "await_activity_result: expected 1 argument, got {}",
                args.len()
            ),
        )
        .unwrap_or(Term::NIL));
    }
    let correlation = match decode_string_arg(args[0]) {
        Ok(value) => value,
        Err(error) => {
            return Ok(
                error_result_term(ctx, &format!("await_activity_result id: {error}"))
                    .unwrap_or(Term::NIL),
            );
        }
    };
    let Some(pid) = ctx.pid() else {
        return Ok(
            error_result_term(ctx, "await_activity_result: missing calling process pid")
                .unwrap_or(Term::NIL),
        );
    };
    let state = match super::nif_state::engine_nif_state(ctx) {
        Ok(state) => state,
        Err(error) => return Ok(error_result_term(ctx, &error).unwrap_or(Term::NIL)),
    };
    let runtime = match runtime_context(&state) {
        Ok(runtime) => runtime,
        Err(error) => return Ok(context_error_term(ctx, &error)),
    };
    let context = match NifContext::new(
        pid,
        runtime.registry.as_ref(),
        runtime.tokio_handle,
        runtime.runtime.signal_delivery(),
    ) {
        Ok(context) => context,
        Err(error) => return Ok(context_error_term(ctx, &error)),
    };
    await_activity_result_with_context(&state, context, &runtime.runtime, ctx, &correlation)
}

fn decode_dispatch_args(args: &[Term]) -> Result<(String, String, String), ()> {
    if args.len() != 3 {
        return Err(());
    }
    let name = decode_string_arg(args[0]).map_err(|_| ())?;
    let input = decode_string_arg(args[1]).map_err(|_| ())?;
    let config = decode_string_arg(args[2]).map_err(|_| ())?;
    Ok((name, input, config))
}

/// First delivery: a dispatch for an ordinal with no recorded attempt trail
/// is attempt 1.
///
/// The remote-tier retry loop ([`spawn_completion_task`], #197) re-dispatches
/// with the incremented attempt when the SDK-declared retry policy (decoded
/// from the dispatch `config` JSON by [`super::nif_activity_retry`]) has
/// budget left, and a live re-dispatch after a crash continues the recorded
/// trail via [`super::nif_activity_retry::next_delivery_attempt`]. This is
/// the single documented producer-side constant; no consumer guesses an
/// attempt.
///
/// In-VM retry-seam constraint: unlike remote activities, an in-VM retry must
/// be driven from a seam that HOLDS THE RUNNER — the dispatch NIF itself
/// (replay's reopen path re-supplies the thunk on every re-execution of
/// workflow code) or an SDK-level loop. The remote retry loop deliberately
/// does not cover the in-VM tier, whose dispatches stay single-attempt.
pub(super) const FIRST_DELIVERY_ATTEMPT: u32 = 1;

/// Grouped parameters for the activity being dispatched.
///
/// Shared with the `collect_*` fan-out natives, which dispatch N of these
/// through the same completion-task machinery.
pub(super) struct ActivityCall {
    pub(super) name: String,
    pub(super) input: String,
    pub(super) config: String,
    /// One-based delivery attempt stamped onto the dispatch (and from there
    /// onto the worker wire). See [`FIRST_DELIVERY_ATTEMPT`].
    pub(super) attempt: u32,
}

fn dispatch_activity_with_context(
    ctx: &mut ProcessContext,
    mut context: NifContext,
    dispatcher: Option<Arc<dyn ActivityDispatcher>>,
    runtime: Arc<crate::RuntimeHandle>,
    tokio_handle: &tokio::runtime::Handle,
    call: ActivityCall,
) -> Result<Term, Term> {
    let input_payload = json_payload(ctx, &call.input, "dispatch_activity", "input")?;
    let ordinal = context.next_activity_ordinal();
    let key = CorrelationKey::Activity(ordinal);
    let activity_id = ActivityId::from_sequence_position(ordinal);
    let correlation = correlation_id(ordinal);
    let namespace = context.workflow_handle().namespace().to_owned();
    match context
        .resolve_command(Command::RunActivity {
            key,
            activity_type: call.name.clone(),
            input: input_payload.clone(),
        })
        .map_err(|error| context_error_term(ctx, &error))?
    {
        ResolveOutcome::Recorded(_) => {
            Ok(ok_result_term(ctx, correlation.as_bytes()).unwrap_or(Term::NIL))
        }
        ResolveOutcome::ResumeLive => {
            let Some(dispatcher) = dispatcher else {
                return Ok(error_result_term(
                    ctx,
                    "no activity dispatcher configured — set one via EngineBuilder::activity_dispatcher",
                )
                .unwrap_or(Term::NIL));
            };
            // NSTQ-4 (+#144): resolve the dispatch's task queue once at this
            // schedule seam (activity override > workflow declared default >
            // the workflow's RECORDED start-time queue > the named default),
            // then stamp the same value onto BOTH the recorded
            // `ActivityScheduled` and the live dispatch so history and routing
            // never diverge. The start-time queue is read from recorded history,
            // so replay re-resolves identically.
            let start_time_task_queue = context.start_time_task_queue();
            let task_queue = super::nif_activity::resolve_task_queue(
                &call.config,
                start_time_task_queue.as_deref(),
            );
            // NODE-4: resolve the OPTIONAL node affinity once at the same seam
            // (activity pin, else None — no workflow default), and stamp the same
            // value onto BOTH the recorded `ActivityScheduled` and the live
            // dispatch so history and routing never diverge.
            let node = super::nif_activity::resolve_node(&call.config);
            // #197: a live re-dispatch continues the ordinal's recorded attempt
            // trail instead of restarting it — a crash-recovery resume after a
            // dangling retryable failure keeps `(workflow, activity, attempt)` a
            // stable identity. A fresh ordinal resolves to `call.attempt`
            // (`FIRST_DELIVERY_ATTEMPT`) exactly as before.
            let attempt =
                super::nif_activity_retry::next_delivery_attempt(context.history(), &activity_id)
                    .max(call.attempt);
            record_started(
                ctx,
                &context,
                activity_id.clone(),
                super::nif_activity::ScheduledActivity {
                    activity_type: call.name.clone(),
                    input: input_payload,
                    task_queue: task_queue.clone(),
                    node: node.clone(),
                    // NOI-0: stamp the SAME one-based attempt onto the recorded `ActivityStarted`
                    // that is stamped onto the live `ActivityDispatch` below, so history and the
                    // worker wire agree.
                    attempt,
                },
            )?;
            let labels = labels_from_config(&call.config);
            let request = ActivityDispatch {
                namespace,
                task_queue,
                node,
                workflow_id: context.workflow_id().clone(),
                activity_id,
                name: call.name,
                input: call.input,
                config: call.config,
                attempt,
                labels,
            };
            spawn_completion_task(
                tokio_handle,
                runtime,
                dispatcher,
                RetryRecorderSeam {
                    recorder: context.recorder(),
                    run_id: context.workflow_handle().run_id().clone(),
                },
                context.pid(),
                correlation.clone(),
                request,
            );
            Ok(ok_result_term(ctx, correlation.as_bytes()).unwrap_or(Term::NIL))
        }
    }
}

#[cfg(test)]
use super::nif_activity_retry_dispatch::{RetryLoopTerminal, dispatch_with_retries};
pub(super) use super::nif_activity_retry_dispatch::{RetryRecorderSeam, spawn_completion_task};

use super::nif_activity_await::await_activity_result_with_context;
#[cfg(test)]
pub(super) use super::nif_activity_await::{ActivityAwaitStep, await_activity_step};

#[cfg(test)]
#[path = "nif_activity_dispatch_tests/mod.rs"]
mod tests;
