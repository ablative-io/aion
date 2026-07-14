//! Typed registry and worker-SDK adapters for shell-node activities.

use aion_worker::{ActivityContext, ActivityRegistry, HandlerFuture};
use dev_brief_worker::handlers;
use dev_brief_worker::shell::Shell;

/// The seven shell-node activities served by the binary.
pub(super) fn shell_registry(shell: Shell) -> Result<ActivityRegistry, aion_worker::WorkerError> {
    ActivityRegistry::new()
        .register_activity(
            "provision_workspace",
            blocking(shell.clone(), handlers::provision),
        )?
        .register_activity("run_gates", blocking(shell.clone(), handlers::run_gates))?
        .register_activity("reset_workspace", blocking(shell.clone(), handlers::reset))?
        .register_activity(
            "verify_gates",
            blocking(shell.clone(), handlers::verify_gates),
        )?
        .register_activity(
            "format_verdict_evidence",
            immediate(handlers::format_verdict_evidence),
        )?
        .register_activity("fold_round", immediate(handlers::fold_round))?
        .register_activity("cleanup_workspace", blocking(shell, handlers::cleanup))
}

/// Move a synchronous shell handler onto the blocking thread pool.
fn blocking<Input, Output>(
    shell: Shell,
    body: fn(&Shell, Input) -> Result<Output, aion_worker::ActivityFailure>,
) -> impl for<'context> Fn(Input, &'context ActivityContext) -> HandlerFuture<'context, Output>
+ Send
+ Sync
+ 'static
where
    Input: Send + 'static,
    Output: Send + 'static,
{
    move |input: Input, context: &ActivityContext| {
        let _ = context;
        let shell = shell.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || body(&shell, input))
                .await
                .map_err(|join_error| {
                    aion_worker::ActivityFailure::terminal(format!(
                        "activity handler task did not complete: {join_error}"
                    ))
                })?
        })
    }
}

/// Adapt a pure synchronous handler without consuming a blocking-pool thread.
fn immediate<Input, Output>(
    body: fn(Input) -> Result<Output, aion_worker::ActivityFailure>,
) -> impl for<'context> Fn(Input, &'context ActivityContext) -> HandlerFuture<'context, Output>
+ Send
+ Sync
+ 'static
where
    Input: Send + 'static,
    Output: Send + 'static,
{
    move |input: Input, context: &ActivityContext| {
        let _ = context;
        Box::pin(async move { body(input) })
    }
}
