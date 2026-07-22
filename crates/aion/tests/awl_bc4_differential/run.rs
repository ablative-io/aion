//! Runs one package through a fresh real engine and returns its durable event
//! trail together with a DETERMINISTIC disposition.
//!
//! Two bindings make the run trustworthy:
//!
//! - Splice binding (r2 finding 1): the caller passes the EXACT entry-module
//!   bytes it expects the engine to load; `run_package` refuses to run unless
//!   the package's entry beam equals them, immediately before `load_workflows`.
//!   The reference run is bound to the reference entry bytes and the direct run
//!   to the `select()` bytes, so swapping `reference.clone()` in for the direct
//!   package fails before execution — the oracle can never compare a package to
//!   itself. `oracle_self_test.rs` exercises exactly that mis-binding.
//!
//! - Park evidence (r2 finding 3): a run is `Completed`/`Failed`/`Cancelled`
//!   the moment its terminal event is recorded, and `Parked` only on POSITIVE
//!   evidence — the engine's visibility surface reports the workflow `Running`
//!   (non-terminal) AND the history carries a specific `TimerStarted` with no
//!   matching `TimerFired`. A stability read is kept only as a supplementary
//!   guard. Bare signal waits expose no positive registration evidence through
//!   any engine surface, so signal-parking fixtures are excluded from the
//!   oracle upstream (see `covered.rs`), never inferred from silence.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aion::EngineBuilder;
use aion_core::{Event, Payload, TimerId, WorkflowFilter, WorkflowId, WorkflowStatus};
use aion_package::Package;
use aion_store::{EventStore, InMemoryStore};
use serde_json::Value;
use uuid::Uuid;

use crate::dispatcher::TypedDispatcher;

/// Hard upper bound on a single run. No fixture that reaches a terminal or a
/// durable-timer park needs anywhere near this long; exceeding it is `Stuck`.
const RUN_DEADLINE: Duration = Duration::from_secs(15);

/// History poll interval.
const POLL: Duration = Duration::from_millis(20);

/// Supplementary stability guard: consecutive unchanged reads that must
/// accompany the positive park evidence before a park is accepted.
const STABLE_READS: u32 = 3;

/// The recorded disposition of a run, derived entirely from durable history plus
/// the engine's visibility status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    /// A terminal `WorkflowCompleted` was recorded.
    Completed,
    /// A terminal `WorkflowFailed` was recorded.
    Failed,
    /// A terminal `WorkflowCancelled` was recorded.
    Cancelled,
    /// Positive durable-timer park: visibility `Running`, a pending
    /// `TimerStarted`, no terminal event.
    Parked,
    /// Neither terminal nor positively parked within the deadline (infra).
    Stuck,
}

/// The fixed workflow id both backends run under. Giving the reference and
/// direct runs the SAME identity is what "the same workflow, two byte
/// productions" means: a fixture that routes its own `workflow.id` into its
/// output then produces the identical result on both sides, controlling identity
/// at the experiment's inputs rather than in the normalizer (decision 11). Each
/// run uses a fresh in-memory store, so one shared id never collides.
pub fn shared_workflow_id() -> WorkflowId {
    WorkflowId::new(Uuid::from_u128(0x00bc_4d1f_0000_0000_0000_0000_0000_0001))
}

/// The outcome of one run: its durable trail, disposition, and — as positive
/// park evidence — the pending durable timers observed at quiescence.
pub struct RunOutcome {
    /// The durable event trail (full when terminal, quiescent-partial when
    /// parked).
    pub trail: Vec<Event>,
    /// The disposition derived from history + visibility.
    pub disposition: Disposition,
    /// The `Display` forms of every `TimerStarted` with no matching
    /// `TimerFired` — the identity of the timer(s) the run is parked on.
    pub pending_timers: Vec<String>,
}

/// Runs `package`'s `workflow_type` on `input` through a fresh engine, refusing
/// unless the package's entry beam equals `expected_entry` (splice binding), and
/// returns the recorded trail plus its disposition.
///
/// # Errors
///
/// Fails when the loaded package's entry beam does not match `expected_entry`,
/// or when the engine cannot be built/started or history cannot be read — every
/// such error is infrastructure, surfaced by the caller, never a disposition.
pub async fn run_package(
    package: Package,
    workflow_type: &str,
    input: &Value,
    action_results: HashMap<String, String>,
    expected_entry: &[u8],
) -> Result<RunOutcome, Box<dyn std::error::Error>> {
    // Splice binding: the package about to be loaded MUST carry the expected
    // production's entry bytes. This ties the differential's comparison to the
    // actual bytes each engine loads.
    let entry_module = package.manifest().entry_module.clone();
    let loaded_entry = package
        .beams()
        .get(&entry_module)
        .ok_or_else(|| format!("package has no entry beam `{entry_module}`"))?;
    if loaded_entry != expected_entry {
        return Err(format!(
            "splice binding violated: the package's entry beam `{entry_module}` \
             ({} bytes) is not the expected production ({} bytes) — refusing to \
             run a package the oracle did not bind",
            loaded_entry.len(),
            expected_entry.len()
        )
        .into());
    }

    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .activity_dispatcher(Arc::new(TypedDispatcher::new(action_results)))
        .load_workflows(package)
        .build()
        .await?;
    let handle = engine
        .start_workflow_with_id(
            workflow_type,
            Payload::from_json(input)?,
            HashMap::new(),
            String::from("default"),
            Some(shared_workflow_id()),
            None,
        )
        .await?;

    let deadline = Instant::now() + RUN_DEADLINE;
    let mut previous: Option<Vec<Event>> = None;
    let mut stable = 0u32;
    let outcome = loop {
        let history = store.read_history(handle.workflow_id()).await?;
        if let Some(disposition) = terminal_disposition(&history) {
            break outcome(history, disposition);
        }
        let pending = pending_timers(&history);
        stable = if previous.as_ref() == Some(&history) {
            stable + 1
        } else {
            0
        };
        // Positive park evidence: a specific pending timer AND visibility
        // reports the workflow Running, confirmed by a supplementary stable read.
        if !pending.is_empty()
            && stable >= STABLE_READS
            && is_running(&engine, workflow_type, handle.workflow_id()).await?
        {
            break outcome(history, Disposition::Parked);
        }
        if Instant::now() >= deadline {
            break outcome(history, Disposition::Stuck);
        }
        previous = Some(history);
        tokio::time::sleep(POLL).await;
    };
    engine.shutdown()?;
    Ok(outcome)
}

/// Assembles a run outcome, recording the pending-timer identities as evidence.
fn outcome(trail: Vec<Event>, disposition: Disposition) -> RunOutcome {
    let pending_timers = pending_timers(&trail)
        .into_iter()
        .map(|timer| timer.to_string())
        .collect();
    RunOutcome {
        trail,
        disposition,
        pending_timers,
    }
}

/// Whether the engine's visibility surface reports this workflow as `Running`
/// (non-terminal) — the positive "actively parked, not finished" signal.
async fn is_running(
    engine: &aion::Engine,
    workflow_type: &str,
    workflow_id: &WorkflowId,
) -> Result<bool, Box<dyn std::error::Error>> {
    let filter = WorkflowFilter {
        workflow_type: Some(workflow_type.to_owned()),
        status: None,
        started_after: None,
        started_before: None,
        parent: None,
    };
    let summaries = engine.list_workflows(filter).await?;
    Ok(summaries.iter().any(|summary| {
        &summary.workflow_id == workflow_id && summary.status == WorkflowStatus::Running
    }))
}

/// Returns the terminal disposition when the history carries a terminal workflow
/// event, else `None`.
fn terminal_disposition(history: &[Event]) -> Option<Disposition> {
    history.iter().find_map(|event| match event {
        Event::WorkflowCompleted { .. } => Some(Disposition::Completed),
        Event::WorkflowFailed { .. } => Some(Disposition::Failed),
        Event::WorkflowCancelled { .. } => Some(Disposition::Cancelled),
        _ => None,
    })
}

/// The `TimerId`s of every `TimerStarted` with no matching `TimerFired` — the
/// pending durable timers a run is parked on.
pub fn pending_timers(history: &[Event]) -> Vec<TimerId> {
    let fired: Vec<&TimerId> = history
        .iter()
        .filter_map(|event| match event {
            Event::TimerFired { timer_id, .. } => Some(timer_id),
            _ => None,
        })
        .collect();
    history
        .iter()
        .filter_map(|event| match event {
            Event::TimerStarted { timer_id, .. } if !fired.contains(&timer_id) => {
                Some(timer_id.clone())
            }
            _ => None,
        })
        .collect()
}
