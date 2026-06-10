//! Two-phase activity dispatch NIFs.

use std::sync::Arc;

use crate::activity::bridge::{ActivityDispatcher, activity_dispatcher};
use crate::durability::{Command, CorrelationKey, Resolution, ResolveOutcome};
use crate::runtime::nif_activity::{
    activity_error, activity_id_from_correlation, context_error_term, correlation_id,
    decode_string_arg, error_result_term, json_payload, ok_result_term, record_completed,
    record_failed, record_started, runtime_context,
};
use crate::runtime::nif_context::NifContext;
use aion_core::ActivityId;
use beamr::native::ProcessContext;
use beamr::term::Term;
use futures::FutureExt;

/// NIF backing `aion_flow_ffi:dispatch_activity/3`.
pub(super) fn dispatch_activity_impl(
    args: &[Term],
    ctx: &mut ProcessContext,
) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    let Ok((name, input, config)) = decode_dispatch_args(args) else {
        return Ok(error_result_term(&format!(
            "dispatch_activity: expected 3 arguments, got {}",
            args.len()
        ))
        .unwrap_or(Term::NIL));
    };
    let Some(pid) = ctx.pid() else {
        return Ok(
            error_result_term("dispatch_activity: missing calling process pid")
                .unwrap_or(Term::NIL),
        );
    };
    let runtime = match runtime_context() {
        Ok(runtime) => runtime,
        Err(error) => return Ok(context_error_term(&error)),
    };
    let context =
        match NifContext::new(pid, runtime.registry.as_ref(), runtime.tokio_handle.clone()) {
            Ok(context) => context,
            Err(error) => return Ok(context_error_term(&error)),
        };
    let dispatcher = activity_dispatcher();
    dispatch_activity_with_context(
        context,
        dispatcher,
        runtime.runtime,
        &runtime.tokio_handle,
        name,
        input,
        config,
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
        return Ok(error_result_term(&format!(
            "await_activity_result: expected 1 argument, got {}",
            args.len()
        ))
        .unwrap_or(Term::NIL));
    }
    let correlation = match decode_string_arg(args[0]) {
        Ok(value) => value,
        Err(error) => {
            return Ok(
                error_result_term(&format!("await_activity_result id: {error}"))
                    .unwrap_or(Term::NIL),
            );
        }
    };
    let Some(pid) = ctx.pid() else {
        return Ok(
            error_result_term("await_activity_result: missing calling process pid")
                .unwrap_or(Term::NIL),
        );
    };
    let runtime = match runtime_context() {
        Ok(runtime) => runtime,
        Err(error) => return Ok(context_error_term(&error)),
    };
    let context = match NifContext::new(pid, runtime.registry.as_ref(), runtime.tokio_handle) {
        Ok(context) => context,
        Err(error) => return Ok(context_error_term(&error)),
    };
    await_activity_result_with_context(context, &runtime.runtime, ctx, &correlation)
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

/// Grouped parameters for the activity being dispatched.
struct ActivityCall {
    name: String,
    input: String,
    config: String,
}

fn dispatch_activity_with_context(
    mut context: NifContext,
    dispatcher: Option<Arc<dyn ActivityDispatcher>>,
    runtime: Arc<crate::RuntimeHandle>,
    tokio_handle: &tokio::runtime::Handle,
    name: String,
    input_text: String,
    config: String,
) -> Result<Term, Term> {
    let input_payload = json_payload(&input_text, "dispatch_activity", "input")?;
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
        .map_err(|error| context_error_term(&error))?
    {
        ResolveOutcome::Recorded(_) => {
            Ok(ok_result_term(correlation.as_bytes()).unwrap_or(Term::NIL))
        }
        ResolveOutcome::ResumeLive => {
            let Some(dispatcher) = dispatcher else {
                return Ok(error_result_term(
                    "no activity dispatcher configured — set one via EngineBuilder::activity_dispatcher",
                )
                .unwrap_or(Term::NIL));
            };
            record_started(&context, activity_id, name.clone(), input_payload)?;
            let call = ActivityCall {
                name,
                input: input_text,
                config,
            };
            spawn_completion_task(
                tokio_handle,
                runtime,
                dispatcher,
                context.pid(),
                correlation.clone(),
                call,
            );
            Ok(ok_result_term(correlation.as_bytes()).unwrap_or(Term::NIL))
        }
    }
}

fn spawn_completion_task(
    tokio_handle: &tokio::runtime::Handle,
    runtime: Arc<crate::RuntimeHandle>,
    dispatcher: Arc<dyn ActivityDispatcher>,
    workflow_pid: u64,
    correlation_id: String,
    call: ActivityCall,
) {
    let future = futures::future::lazy(move |_| {
        dispatcher.dispatch_from_process(&call.name, &call.input, &call.config, Some(workflow_pid))
    })
    .map(move |result| {
        match result {
            Ok(payload) => {
                if let Err(error) = runtime.deliver_activity_completion_message(
                    workflow_pid,
                    &correlation_id,
                    payload,
                ) {
                    tracing::warn!(%error, workflow_pid, correlation_id, "activity completion delivery failed");
                }
            }
            Err(reason) => {
                if let Err(error) = runtime.deliver_activity_failure_message(
                    workflow_pid,
                    &correlation_id,
                    reason,
                ) {
                    tracing::warn!(%error, workflow_pid, correlation_id, "activity failure delivery failed");
                }
            }
        }
    });
    tokio_handle.spawn(future);
}

fn await_activity_result_with_context(
    mut context: NifContext,
    runtime: &Arc<crate::RuntimeHandle>,
    process_context: &mut ProcessContext,
    correlation: &str,
) -> Result<Term, Term> {
    let activity_id = activity_id_from_correlation(correlation)?;
    if let Some(recorded) = recorded_resolution_for(&mut context, &activity_id)? {
        return Ok(recorded_result_term(recorded));
    }
    if let Some(term) = live_completion_term(&context, runtime, activity_id, process_context)? {
        return Ok(term);
    }
    process_context.request_suspend(None);
    Ok(Term::NIL)
}

fn recorded_resolution_for(
    context: &mut NifContext,
    activity_id: &ActivityId,
) -> Result<Option<Resolution>, Term> {
    let ordinal = activity_id.sequence_position();
    let input = json_payload("null", "await_activity_result", "replay input")?;
    match context
        .resolve_command(Command::RunActivity {
            key: CorrelationKey::Activity(ordinal),
            activity_type: "await_activity_result".to_owned(),
            input,
        })
        .map_err(|error| context_error_term(&error))?
    {
        ResolveOutcome::Recorded(resolution) => Ok(Some(resolution)),
        ResolveOutcome::ResumeLive => Ok(None),
    }
}

fn recorded_result_term(resolution: Resolution) -> Term {
    match resolution {
        Resolution::ActivityCompleted(payload) => {
            ok_result_term(payload.bytes()).unwrap_or(Term::NIL)
        }
        Resolution::ActivityFailedTerminal(error) => {
            error_result_term(&error.message).unwrap_or(Term::NIL)
        }
        other => error_result_term(&format!(
            "await_activity_result: recorded non-activity resolution {other:?}"
        ))
        .unwrap_or(Term::NIL),
    }
}

fn live_completion_term(
    context: &NifContext,
    runtime: &crate::RuntimeHandle,
    activity_id: ActivityId,
    process_context: &mut ProcessContext,
) -> Result<Option<Term>, Term> {
    let Some(select) = process_context.select_facility() else {
        return take_runtime_completion(context, runtime, activity_id);
    };
    for index in 0..select.message_count() {
        let Some(message) = select.peek_message(index) else {
            continue;
        };
        if completion_marker(
            message,
            runtime,
            context.pid(),
            activity_id.sequence_position(),
        ) {
            select.remove_message(index);
            return take_runtime_completion(context, runtime, activity_id);
        }
    }
    Ok(None)
}

fn completion_marker(
    message: Term,
    runtime: &crate::RuntimeHandle,
    workflow_pid: u64,
    activity_sequence: u64,
) -> bool {
    let complete = runtime.activity_complete_atom();
    let failed = runtime.activity_failed_atom();
    if message == Term::atom(complete) {
        return runtime
            .activity_result(workflow_pid, activity_sequence)
            .is_some();
    }
    message == Term::atom(failed)
        && runtime
            .activity_error(workflow_pid, activity_sequence)
            .is_some()
}

fn take_runtime_completion(
    context: &NifContext,
    runtime: &crate::RuntimeHandle,
    activity_id: ActivityId,
) -> Result<Option<Term>, Term> {
    if let Some(payload) =
        runtime.take_activity_result(context.pid(), activity_id.sequence_position())
    {
        record_completed(context, activity_id, payload.clone())?;
        return Ok(Some(ok_result_term(payload.bytes()).unwrap_or(Term::NIL)));
    }
    if let Some(error) = runtime.take_activity_error(context.pid(), activity_id.sequence_position())
    {
        record_failed(context, activity_id, activity_error(error.message.clone()))?;
        return Ok(Some(error_result_term(&error.message).unwrap_or(Term::NIL)));
    }
    Ok(None)
}
