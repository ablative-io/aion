//! Agent-step worker for the `{{name}}` durable agent loop.
//!
//! Serves the three parameterised agent steps the workflow dispatches:
//! `scout`, `act`, and `verify`. Each receives a `StepInput` carrying the
//! step's prompt and the prior step's output as `context`, and returns a
//! `StepOutput` with the step's result.
//!
//! This scaffold bundles NO agent runtime: the handlers below are trivial
//! echoes that prove the loop runs end to end. Replace each body with your
//! own driver — an LLM call, a tool loop, a `norn` agent — keeping the
//! `StepInput` -> `StepOutput` contract. The runtime stays here, worker-side;
//! the workflow only records each step's outcome for durable replay.
//!
//! The `StepInput`/`StepOutput` structs mirror the boundary types authored in
//! `src/{{name}}_io.gleam` (the types-first source of truth `aion generate`
//! derives the workflow's codecs and schemas from). If you change a step
//! type, update both sides and rerun `aion generate`.
//!
//! Input types require `Serialize + DeserializeOwned`; output types require
//! `Serialize`. A handler fails with `ActivityFailure::retryable(..)` for a
//! transient fault the workflow may retry, or `ActivityFailure::terminal(..)`
//! for one it must not — the worker never retries on its own.

use std::time::Duration;

use aion_worker::{ActivityContext, HandlerFuture, Worker, WorkerConfig};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct StepInput {
    task_id: String,
    prompt: String,
    context: String,
}

#[derive(Serialize)]
struct StepOutput {
    result: String,
}

/// Survey the task. Replace the body with your agent's scouting pass — the
/// `prompt` is the instruction; `context` is empty for the first step.
fn scout(input: StepInput, _context: &ActivityContext) -> HandlerFuture<'_, StepOutput> {
    Box::pin(async move {
        println!("scout {}: {}", input.task_id, input.prompt);
        Ok(StepOutput {
            result: format!("scouted({})", input.prompt),
        })
    })
}

/// Take the action the scout identified. `context` carries the scout result.
fn act(input: StepInput, _context: &ActivityContext) -> HandlerFuture<'_, StepOutput> {
    Box::pin(async move {
        println!(
            "act {}: {} over {}",
            input.task_id, input.prompt, input.context
        );
        Ok(StepOutput {
            result: format!("acted({}|{})", input.prompt, input.context),
        })
    })
}

/// Verify the artifact the action produced. `context` carries the act result.
fn verify(input: StepInput, _context: &ActivityContext) -> HandlerFuture<'_, StepOutput> {
    Box::pin(async move {
        println!(
            "verify {}: {} over {}",
            input.task_id, input.prompt, input.context
        );
        Ok(StepOutput {
            result: format!("verified({}|{})", input.prompt, input.context),
        })
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Every field is explicit; there are no hidden defaults. The endpoint
    // matches the gRPC address in this project's aion.toml.
    let config = WorkerConfig::builder()
        .endpoint("http://127.0.0.1:50051")
        .task_queue("default")
        .identity("{{name}}-worker-1")
        .max_concurrency(4)
        .reconnect_initial_backoff(Duration::from_millis(100))
        .reconnect_max_backoff(Duration::from_secs(5))
        .reconnect_max_attempts(10)
        .build()?;

    Worker::builder(config)
        .register_activity("scout", scout)?
        .register_activity("act", act)?
        .register_activity("verify", verify)?
        .build()?
        .run()
        .await?;

    Ok(())
}
