//! Typed registry and worker-SDK adapters for shell-node activities.

use aion_worker::{ActivityContext, ActivityRegistry, HandlerFuture};
use staged_rounds_worker::handlers;
use staged_rounds_worker::shell::Shell;

/// The three shell-node activities served by the binary.
pub(super) fn shell_registry(shell: Shell) -> Result<ActivityRegistry, aion_worker::WorkerError> {
    ActivityRegistry::new()
        .register_activity(
            "provision_item",
            blocking(shell.clone(), handlers::provision_item),
        )?
        .register_activity("merge_branches", blocking(shell, handlers::merge_branches))?
        .register_activity("fold_phase", immediate(handlers::fold_phase))
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
