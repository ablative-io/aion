//! Durable continue-as-new NIF implementation.

use beamr::native::ProcessContext;
use beamr::term::Term;
use chrono::Utc;

use crate::runtime::nif_activity::{
    context_error_term, decode_string_arg, error_result_term, json_payload, ok_result_term,
    runtime_context,
};
use crate::runtime::nif_context::NifContext;

/// Record `WorkflowContinuedAsNew` through the current workflow recorder and
/// terminate the current workflow process so the lifecycle monitor can start the
/// replacement run from the terminal history event.
pub(crate) fn continue_as_new_impl(
    args: &[Term],
    process_context: &mut ProcessContext,
) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if args.len() != 1 {
        return Ok(error_result_term(&format!(
            "continue_as_new: expected 1 arguments, got {}",
            args.len()
        ))
        .unwrap_or(Term::NIL));
    }

    let result = continue_as_new(args, process_context);
    match result {
        Ok(()) => Ok(ok_result_term(b"continued_as_new").unwrap_or(Term::NIL)),
        Err(term) => Ok(term),
    }
}

fn continue_as_new(args: &[Term], process_context: &ProcessContext) -> Result<(), Term> {
    let state = crate::runtime::nif_state::engine_nif_state(process_context)
        .map_err(|error| error_result_term(&error).unwrap_or(Term::NIL))?;
    let runtime = runtime_context(&state).map_err(|error| context_error_term(&error))?;
    let pid = process_context.pid().ok_or_else(|| {
        error_result_term("continue_as_new: missing calling pid").unwrap_or(Term::NIL)
    })?;
    // continue_as_new records a terminal event; a query handler must stay
    // read-only.
    crate::runtime::nif_query_pump::ensure_not_servicing_query(&state, pid, "continue_as_new")
        .map_err(|error| error_result_term(&error).unwrap_or(Term::NIL))?;
    let context = NifContext::new(
        pid,
        runtime.registry.as_ref(),
        runtime.tokio_handle.clone(),
        runtime.runtime.signal_delivery(),
    )
    .map_err(|error| context_error_term(&error))?;
    let input_text = decode_string_arg(args[0]).map_err(|error| {
        error_result_term(&format!("continue_as_new input: {error}")).unwrap_or(Term::NIL)
    })?;
    let input = json_payload(&input_text, "continue_as_new", "input")?;
    let parent_run_id = context.run_id().clone();
    let input_for_record = input.clone();

    context
        .block_on_recorder(|recorder| {
            Box::pin(async move {
                // Terminal check and terminal record are atomic under the
                // recorder lock: a concurrent cancel/complete/fail transition
                // records through the same recorder, and continuing a run that
                // already has a terminal event would corrupt its history with
                // a second terminal.
                let history = recorder.read_history().await?;
                if crate::lifecycle::completion::terminal_outcome_from_history(
                    &history,
                    &parent_run_id,
                )
                .is_some()
                {
                    return Err(crate::durability::DurabilityError::HistoryShape {
                        reason: format!(
                            "continue_as_new rejected: run {parent_run_id} already recorded a terminal event"
                        ),
                    });
                }
                recorder
                    .record_workflow_continued_as_new(
                        Utc::now(),
                        input_for_record,
                        None,
                        parent_run_id,
                    )
                    .await
            })
        })
        .map_err(|error| context_error_term(&error))?;

    runtime.runtime.cancel_pid(context.pid()).map_err(|error| {
        error_result_term(&format!("continue_as_new termination failed: {error}"))
            .unwrap_or(Term::NIL)
    })?;

    Ok(())
}
