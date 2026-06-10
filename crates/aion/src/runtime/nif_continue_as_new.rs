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
    let runtime = runtime_context().map_err(|error| context_error_term(&error))?;
    let pid = process_context.pid().ok_or_else(|| {
        error_result_term("continue_as_new: missing calling pid").unwrap_or(Term::NIL)
    })?;
    let context = NifContext::new(pid, runtime.registry.as_ref(), runtime.tokio_handle.clone())
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
