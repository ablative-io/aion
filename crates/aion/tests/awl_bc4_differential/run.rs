//! Runs one package through a fresh real engine and returns its durable event
//! trail.
//!
//! Both backends run through the SAME builder shape (in-memory store +
//! visibility, one scheduler thread, the one shared `EchoDispatcher`), so the
//! trails can only diverge on backend behavior.
//!
//! The engine fires durable timers on the real wall clock (there is no sim
//! clock), and several fixtures block on a `sleep 30s` or a signal `wait` that
//! no external signal will ever satisfy. Rather than inject signals — which
//! would race the timer/wait ordering and manufacture divergences — the run is
//! bounded by a deadline. A fixture that does not reach a terminal state by the
//! deadline is quiescent at a deterministic point (blocked on its timer or
//! wait), and its PARTIAL trail is compared and reported as `unsettled`. Both
//! backends reach the identical quiescent point, so the comparison stays
//! honest.

use std::sync::Arc;
use std::time::Duration;

use aion::EngineBuilder;
use aion_core::{Event, Payload, WorkflowId};
use aion_package::Package;
use aion_store::{EventStore, InMemoryStore};
use serde_json::Value;
use tokio::time::timeout;
use uuid::Uuid;

use crate::dispatcher::EchoDispatcher;

/// The fixed workflow id both backends run under. Giving the reference and
/// direct runs the SAME identity is what "the same workflow, two byte
/// productions" means: a fixture that routes its own `workflow.id` into its
/// output (the `workflow_id` fixture) then produces the identical result on
/// both sides, so identity leaking into a user payload is controlled at the
/// experiment's inputs rather than papered over in the normalizer (decision
/// 11). Each run uses a fresh in-memory store, so one shared id never
/// collides. The value is arbitrary but stable.
fn shared_workflow_id() -> WorkflowId {
    WorkflowId::new(Uuid::from_u128(0x00bc_4d1f_0000_0000_0000_0000_0000_0001))
}

/// How long a run may take before it is treated as blocked at a deterministic
/// quiescent point. No fixture that reaches a terminal state needs anywhere
/// near this long (the echo dispatcher is synchronous); the only fixtures that
/// hit it block on a 30s+ durable timer or an unsatisfiable signal wait.
const RUN_DEADLINE: Duration = Duration::from_secs(6);

/// Grace period after the deadline for any in-flight step to quiesce before
/// the partial history is read.
const DRAIN: Duration = Duration::from_millis(300);

/// The outcome of one run: its durable trail and whether it reached a terminal
/// state within the deadline.
pub struct RunOutcome {
    /// The durable event trail (full when settled, partial when not).
    pub trail: Vec<Event>,
    /// Whether the run reached a terminal state within the deadline.
    pub settled: bool,
}

/// Runs `package`'s `workflow_type` on `input` through a fresh engine and
/// returns the recorded durable trail.
///
/// # Errors
///
/// Fails when the engine cannot be built, the workflow cannot be started, or
/// history cannot be read. A run that fails to SETTLE is not an error — it is
/// reported via [`RunOutcome::settled`].
pub async fn run_package(
    package: Package,
    workflow_type: &str,
    input: &Value,
) -> Result<RunOutcome, Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .activity_dispatcher(Arc::new(EchoDispatcher))
        .load_workflows(package)
        .build()
        .await?;
    let handle = engine
        .start_workflow_with_id(
            workflow_type,
            Payload::from_json(input)?,
            std::collections::HashMap::new(),
            String::from("default"),
            Some(shared_workflow_id()),
            None,
        )
        .await?;

    let settled = matches!(
        timeout(
            RUN_DEADLINE,
            engine.result(handle.workflow_id(), handle.run_id())
        )
        .await,
        Ok(Ok(_))
    );
    if !settled {
        // Let any in-flight step land, then read the quiescent partial trail.
        tokio::time::sleep(DRAIN).await;
    }
    let trail = store.read_history(handle.workflow_id()).await?;
    engine.shutdown()?;
    Ok(RunOutcome { trail, settled })
}
