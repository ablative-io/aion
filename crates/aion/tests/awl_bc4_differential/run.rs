//! Runs one package through a fresh real engine and returns its durable event
//! trail together with a DETERMINISTIC disposition.
//!
//! Both backends run through the SAME builder shape (in-memory store +
//! visibility, one scheduler thread, the one shared typed dispatcher), so the
//! trails can only diverge on backend behavior.
//!
//! The disposition is read from the recorded history, never from a single
//! wall-clock sample: the run is `Completed`/`Failed`/`Cancelled` the moment a
//! terminal event is recorded, and `Parked` only once the history has stopped
//! changing — byte-stable across several consecutive reads with no terminal
//! event — i.e. the workflow is genuinely quiescent at a durable boundary
//! (a `sleep`/`wait … timeout` timer, or a bare signal `wait`). Detection is
//! event-driven, not a fixed drain: a run still producing events keeps
//! resetting the stability counter, so a lucky early-prefix read can never be
//! mistaken for a park. Whether the park carries a pending durable timer is
//! reported as positive evidence (`timer_pending`) so the two park kinds are
//! pinned distinctly. A run that never quiesces or terminates within the hard
//! deadline is `Stuck` — an infrastructure failure, never a silent "unsettled".

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aion::EngineBuilder;
use aion_core::{Event, Payload, TimerId, WorkflowId};
use aion_package::Package;
use aion_store::{EventStore, InMemoryStore};
use serde_json::Value;
use uuid::Uuid;

use crate::dispatcher::TypedDispatcher;

/// Hard upper bound on a single run. No fixture that reaches a terminal state
/// needs anywhere near this long (the dispatcher is synchronous); the only
/// fixtures that approach it are those parked on a 30s+ durable timer, which
/// are detected as `Parked` well before it. Exceeding it is a hard failure.
const RUN_DEADLINE: Duration = Duration::from_secs(15);

/// History poll interval.
const POLL: Duration = Duration::from_millis(20);

/// Consecutive unchanged non-terminal reads that confirm quiescence. At the
/// `POLL` cadence this is a settle window (~120ms) far longer than any
/// inter-event gap the synchronous dispatcher produces, so a mid-execution
/// scheduling gap can never masquerade as a park, while a genuinely parked
/// workflow (no more events will ever come) confirms quickly.
const STABLE_READS: u32 = 6;

/// The recorded disposition of a run, derived entirely from durable history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    /// A terminal `WorkflowCompleted` was recorded.
    Completed,
    /// A terminal `WorkflowFailed` was recorded.
    Failed,
    /// A terminal `WorkflowCancelled` was recorded.
    Cancelled,
    /// Quiescent at a durable boundary: no terminal event and a history that
    /// has stopped changing (a durable timer or a bare signal wait).
    Parked,
    /// Neither terminal nor stably parked within the deadline (infra failure).
    Stuck,
}

/// The fixed workflow id both backends run under. Giving the reference and
/// direct runs the SAME identity is what "the same workflow, two byte
/// productions" means: a fixture that routes its own `workflow.id` into its
/// output then produces the identical result on both sides, controlling
/// identity at the experiment's inputs rather than in the normalizer
/// (decision 11). Each run uses a fresh in-memory store, so one shared id never
/// collides. The value is arbitrary but stable.
fn shared_workflow_id() -> WorkflowId {
    WorkflowId::new(Uuid::from_u128(0x00bc_4d1f_0000_0000_0000_0000_0000_0001))
}

/// The outcome of one run: its durable trail, recorded disposition, and — as
/// positive park evidence — whether a durable timer was pending at quiescence.
pub struct RunOutcome {
    /// The durable event trail (full when terminal, quiescent-partial when
    /// parked).
    pub trail: Vec<Event>,
    /// The disposition derived from that trail.
    pub disposition: Disposition,
    /// Whether the trail carries a `TimerStarted` with no matching `TimerFired`
    /// — the positive evidence distinguishing a timer park from a signal-wait
    /// park.
    pub timer_pending: bool,
}

/// Runs `package`'s `workflow_type` on `input` through a fresh engine, using
/// `action_results` to answer every dispatched activity, and returns the
/// recorded trail plus its disposition.
///
/// # Errors
///
/// Fails when the engine cannot be built, the workflow cannot be started, or
/// history cannot be read — every such error is infrastructure, surfaced by the
/// caller, never folded into a run disposition.
pub async fn run_package(
    package: Package,
    workflow_type: &str,
    input: &Value,
    action_results: HashMap<String, String>,
) -> Result<RunOutcome, Box<dyn std::error::Error>> {
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
        if previous.as_ref().is_some_and(|prior| prior == &history) {
            stable += 1;
        } else {
            stable = 0;
        }
        if stable >= STABLE_READS {
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

/// Assembles a run outcome, computing the park evidence from the trail.
fn outcome(trail: Vec<Event>, disposition: Disposition) -> RunOutcome {
    let timer_pending = timer_pending(&trail);
    RunOutcome {
        trail,
        disposition,
        timer_pending,
    }
}

/// Returns the terminal disposition when the history carries a terminal
/// workflow event, else `None`.
fn terminal_disposition(history: &[Event]) -> Option<Disposition> {
    history.iter().find_map(|event| match event {
        Event::WorkflowCompleted { .. } => Some(Disposition::Completed),
        Event::WorkflowFailed { .. } => Some(Disposition::Failed),
        Event::WorkflowCancelled { .. } => Some(Disposition::Cancelled),
        _ => None,
    })
}

/// Whether the history carries at least one durable timer that started and has
/// not fired — the park evidence for a `sleep`/`wait … timeout` boundary.
pub fn timer_pending(history: &[Event]) -> bool {
    let fired: Vec<&TimerId> = history
        .iter()
        .filter_map(|event| match event {
            Event::TimerFired { timer_id, .. } => Some(timer_id),
            _ => None,
        })
        .collect();
    history.iter().any(
        |event| matches!(event, Event::TimerStarted { timer_id, .. } if !fired.contains(&timer_id)),
    )
}
