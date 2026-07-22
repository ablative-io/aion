//! Timer NIF implementations for the `aion_flow_ffi` namespace.

use std::time::Duration;

use aion_core::{Event, TimerId};
use beamr::native::ProcessContext;
use beamr::term::Term;
use chrono::{DateTime, Utc};

use crate::durability::{Command, CorrelationKey, DurabilityError, Resolution, ResolveOutcome};
use crate::runtime::engine_nifs::{decode_string_arg, error_result_term, ok_result_term};
use crate::runtime::nif_context::{NifContext, NifContextError};
use crate::runtime::nif_state::{EngineNifState, PendingAwait};
use crate::runtime::nif_timer_bridge::{run_blocking, timer_bridge};
use crate::time;

/// NIF backing `aion_flow_ffi:sleep/1`.
///
/// Two-phase suspend: the native never blocks a scheduler thread. On first
/// live arrival it records `TimerStarted`, schedules the durable timer, pins
/// the await identity, and parks the process via `request_suspend`. Every
/// mailbox wake re-enters this native, which consumes one wake marker and
/// re-runs the resolution: a recorded `TimerFired`/`TimerCancelled` settles
/// the sleep; an expired enclosing `with_timeout` deadline cancels it
/// durably; anything else parks again.
pub(super) fn sleep_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    // Queries first (Q6): the entry check runs on every invocation — fresh
    // and wake re-entry — before this await's own resolution. The servicing
    // refusal precedes the marker consumption so a refused handler call
    // never eats a wake; the recording guard itself re-fires inside
    // `timer_call` for the non-sleep timer NIFs.
    if let Ok(state) = super::nif_state::engine_nif_state(ctx)
        && let Some(pid) = ctx.pid()
    {
        if let Err(error) = super::nif_query_pump::ensure_not_servicing_query(&state, pid, "sleep")
        {
            return Ok(error_result_term(ctx, &error).unwrap_or(Term::NIL));
        }
        consume_one_wake_marker(ctx);
        if let Some(sentinel) = super::nif_query_pump::take_pending_query_sentinel(&state, pid) {
            return Ok(error_result_term(ctx, &sentinel).unwrap_or(Term::NIL));
        }
    } else {
        consume_one_wake_marker(ctx);
    }
    timer_call("sleep", 1, args, ctx, |state, mut context, args| {
        let pid = context.pid();
        let duration = decode_duration_arg("sleep duration", args[0])?;
        let pinned = match state.pending_awaits.get(&pid).map(|entry| entry.clone()) {
            Some(PendingAwait::Sleep { timer_id, fire_at }) => Some((timer_id, fire_at)),
            Some(PendingAwait::Signal { .. }) => {
                return Err(NifTimerError::Context(
                    "sleep: process is pinned to a pending signal await".to_owned(),
                ));
            }
            Some(PendingAwait::Child { .. }) => {
                return Err(NifTimerError::Context(
                    "sleep: process is pinned to a pending child await".to_owned(),
                ));
            }
            Some(PendingAwait::Collect { .. }) => {
                return Err(NifTimerError::Context(
                    "sleep: process is pinned to a pending collect await".to_owned(),
                ));
            }
            None => None,
        };
        let first_arrival = pinned.is_none();
        let (timer_id, fire_at) = if let Some(parts) = pinned {
            parts
        } else {
            let now = recorded_now(&context);
            let timer_id = TimerId::anonymous(context.next_timer_ordinal());
            (timer_id, add_duration(now, duration)?)
        };
        match context.resolve_command(timer_command(timer_id.clone(), fire_at))? {
            ResolveOutcome::Recorded(Resolution::TimerFired) => {
                state.pending_awaits.remove(&pid);
                Ok(ok_result("fired"))
            }
            ResolveOutcome::Recorded(Resolution::TimerCancelled) => {
                state.pending_awaits.remove(&pid);
                Ok(error_result("cancelled"))
            }
            ResolveOutcome::Recorded(Resolution::TimerStarted) => {
                // The start is recorded but its terminal is not the
                // immediately following event (asynchronous arrivals
                // interleave); read the terminal from the run segment.
                match timer_terminal_recorded(&context, &timer_id) {
                    Some(true) => {
                        state.pending_awaits.remove(&pid);
                        Ok(ok_result("fired"))
                    }
                    Some(false) => {
                        state.pending_awaits.remove(&pid);
                        Ok(error_result("cancelled"))
                    }
                    // Mid-await: a woken re-entry of a live sleep, or
                    // recovery replay of a sleep whose durable timer the
                    // recovery pass re-armed. Park again.
                    None => park_sleep(state, &context, pid, timer_id, fire_at),
                }
            }
            ResolveOutcome::Recorded(_) => Ok(error_result("sleep history mismatch")),
            ResolveOutcome::ResumeLive => {
                if first_arrival {
                    let now = recorded_now(&context);
                    record_started(&context, now, timer_id.clone(), fire_at)?;
                    schedule_sleep_timer(state, &context, duration, now, &timer_id, fire_at)?;
                }
                park_sleep(state, &context, pid, timer_id, fire_at)
            }
        }
    })
}

/// Park the calling process on its pinned sleep, unless an enclosing
/// `with_timeout` deadline already expired — then the sleep is cancelled
/// durably so replay returns the same error.
///
/// The expiry check is a pure function of the caller's resolution snapshot
/// (`context.history()`): a deadline that fired after the snapshot is not
/// observed here — the fired marker re-enters the native, whose fresh
/// snapshot then converges on the recorded truth. The cancel itself is
/// arbitrated by the timer service's first-recorded-wins terminal guard:
/// `cancel` is a silent no-op when `TimerFired` already landed, so the
/// settled branch is re-read from the recorded terminal — never assumed —
/// exactly as `settle_live_scope` does. Returning `cancelled`
/// unconditionally here lost that race: live answered `cancelled` while
/// replay read the recorded `TimerFired` and answered `fired` (N-3).
fn park_sleep(
    state: &EngineNifState,
    context: &NifContext,
    pid: u64,
    timer_id: TimerId,
    fire_at: DateTime<Utc>,
) -> Result<NifResult, NifTimerError> {
    if super::nif_timeout::expired_scope_deadline(state, pid, context.history()).is_some() {
        cancel_live_timer(state, context, timer_id.clone())?;
        state.pending_awaits.remove(&pid);
        // Re-read the recorded terminal: the cancel may have lost the race
        // to a concurrent fire, and history is the truth replay will read.
        let settled = build_context_for_pid(state, pid)?;
        return match timer_terminal_recorded(&settled, &timer_id) {
            Some(true) => Ok(ok_result("fired")),
            Some(false) => Ok(error_result("cancelled")),
            None => Err(NifTimerError::Context(format!(
                "sleep timer {timer_id:?} has no recorded terminal after a durable cancel"
            ))),
        };
    }
    state
        .pending_awaits
        .insert(pid, PendingAwait::Sleep { timer_id, fire_at });
    Ok(NifResult::Suspend)
}

/// Consume one queued aion wake marker before a suspending await re-runs.
///
/// State recovery errors are deliberately not surfaced here: the await body
/// recovers the same state next and reports the failure as its result.
fn consume_one_wake_marker(ctx: &mut ProcessContext) {
    if let Ok(state) = super::nif_state::engine_nif_state(ctx)
        && let Ok(runtime) = super::nif_activity::runtime_context(&state)
    {
        super::nif_wake::consume_wake_marker(ctx, &runtime.runtime);
    }
}

/// NIF backing `aion_flow_ffi:start_timer/2`.
pub(super) fn start_timer_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    timer_call("start_timer", 2, args, ctx, |state, mut context, args| {
        let timer_id = decode_timer_id_arg("start_timer timer_id", args[0])?;
        let duration = decode_duration_arg("start_timer duration", args[1])?;
        let recorded_now = recorded_now(&context);
        let fire_at = add_duration(recorded_now, duration)?;
        match context.resolve_command(timer_command(timer_id.clone(), fire_at))? {
            ResolveOutcome::Recorded(Resolution::TimerStarted | Resolution::TimerFired) => {
                Ok(ok_result(timer_ref(&timer_id)))
            }
            ResolveOutcome::Recorded(Resolution::TimerCancelled) => Ok(error_result("cancelled")),
            ResolveOutcome::Recorded(_) => Ok(error_result("start_timer history mismatch")),
            ResolveOutcome::ResumeLive => {
                record_started(&context, recorded_now, timer_id.clone(), fire_at)?;
                schedule_timer(state, &context, timer_id.clone(), fire_at, TimerKind::Named)?;
                Ok(ok_result(timer_ref(&timer_id)))
            }
        }
    })
}

/// NIF backing `aion_flow_ffi:cancel_timer/1`.
pub(super) fn cancel_timer_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    timer_call("cancel_timer", 1, args, ctx, |state, mut context, args| {
        let timer_id = decode_timer_id_arg("cancel_timer timer_id", args[0])?;
        let recorded_now = recorded_now(&context);
        match context.resolve_command(timer_command(timer_id.clone(), recorded_now))? {
            ResolveOutcome::Recorded(Resolution::TimerCancelled | Resolution::TimerFired) => {
                Ok(ok_result("cancelled"))
            }
            ResolveOutcome::Recorded(_) => Ok(error_result("cancel_timer history mismatch")),
            ResolveOutcome::ResumeLive => {
                cancel_live_timer(state, &context, timer_id)?;
                Ok(ok_result("cancelled"))
            }
        }
    })
}

fn timer_call<F>(
    name: &str,
    arity: usize,
    args: &[Term],
    process_context: &mut ProcessContext,
    f: F,
) -> Result<Term, Term>
where
    F: FnOnce(&EngineNifState, NifContext, &[Term]) -> Result<NifResult, NifTimerError>,
{
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if args.len() != arity {
        return Ok(error_result_term(
            process_context,
            &format!("{name}: expected {arity} arguments, got {}", args.len()),
        )
        .unwrap_or(Term::NIL));
    }
    let state = match super::nif_state::engine_nif_state(process_context) {
        Ok(state) => state,
        Err(error) => return Ok(error_result_term(process_context, &error).unwrap_or(Term::NIL)),
    };
    // Every timer NIF records durable timer events; refuse while the caller
    // is servicing a query so handler misuse never writes history.
    if let Some(pid) = process_context.pid()
        && let Err(error) = super::nif_query_pump::ensure_not_servicing_query(&state, pid, name)
    {
        return Ok(error_result_term(process_context, &error).unwrap_or(Term::NIL));
    }
    match build_context(&state, process_context).and_then(|context| f(&state, context, args)) {
        Ok(NifResult::Ok(value)) => {
            Ok(ok_result_term(process_context, &value).unwrap_or(Term::NIL))
        }
        Ok(NifResult::Error(message)) => {
            Ok(error_result_term(process_context, &message).unwrap_or(Term::NIL))
        }
        Ok(NifResult::Suspend) => {
            // Park the process; the next mailbox wake re-invokes this
            // native from the top with the await identity pinned in
            // `EngineNifState::pending_awaits`. The NIL return is never
            // observed by workflow code.
            process_context.request_suspend(None);
            Ok(Term::NIL)
        }
        Err(error) => {
            Ok(error_result_term(process_context, &error.to_string()).unwrap_or(Term::NIL))
        }
    }
}

fn build_context(
    state: &EngineNifState,
    process_context: &ProcessContext,
) -> Result<NifContext, NifTimerError> {
    let pid = process_context
        .pid()
        .ok_or_else(|| NifTimerError::Context("missing calling pid".to_owned()))?;
    build_context_for_pid(state, pid)
}

/// Build a [`NifContext`] for a workflow process by pid.
pub(super) fn build_context_for_pid(
    state: &EngineNifState,
    pid: u64,
) -> Result<NifContext, NifTimerError> {
    let bridge = timer_bridge(state)?;
    NifContext::new(
        pid,
        bridge.registry.as_ref(),
        bridge.tokio_handle.clone(),
        bridge.birth_wait,
    )
    .map_err(Into::into)
}

pub(super) fn decode_duration_arg(label: &str, term: Term) -> Result<Duration, NifTimerError> {
    let text = decode_string_arg(term).map_err(NifTimerError::Argument)?;
    let millis = text
        .parse::<u64>()
        .map_err(|error| NifTimerError::Argument(format!("{label}: {error}")))?;
    Ok(Duration::from_millis(millis))
}

/// Engine-minted timer-name prefixes an author may never mint.
///
/// `deadline:` is the per-run workflow-timeout deadline; `schedule:` is the
/// schedule coordinator's trigger timer. Both are minted internally via
/// `TimerId::named`, bypassing this author choke point, so refusing them here
/// keeps a workflow's `start_timer`/`cancel_timer` from colliding with — or
/// forging — an engine timer.
const RESERVED_TIMER_NAME_PREFIXES: [&str; 2] = [crate::time::DEADLINE_TIMER_PREFIX, "schedule:"];

fn decode_timer_id_arg(label: &str, term: Term) -> Result<TimerId, NifTimerError> {
    let raw = decode_string_arg(term).map_err(NifTimerError::Argument)?;
    let name = raw.strip_prefix("timer:named:").unwrap_or(raw.as_str());
    author_timer_id(label, name)
}

/// Build a named timer id from an author-supplied name, refusing reserved
/// engine prefixes.
///
/// The single choke point for author timer names: any name under a reserved
/// prefix (`deadline:`, `schedule:`) is refused so workflow code can neither
/// collide with nor forge an engine-minted timer. Empty names are refused by
/// `TimerId::named`.
fn author_timer_id(label: &str, name: &str) -> Result<TimerId, NifTimerError> {
    if let Some(reserved) = RESERVED_TIMER_NAME_PREFIXES
        .into_iter()
        .find(|reserved| name.starts_with(reserved))
    {
        return Err(NifTimerError::Argument(format!(
            "{label}: timer name `{name}` uses the reserved `{reserved}` prefix"
        )));
    }
    TimerId::named(name).map_err(|error| NifTimerError::Argument(format!("{label}: {error}")))
}

fn timer_ref(timer_id: &TimerId) -> &str {
    timer_id.name().unwrap_or("anonymous")
}

pub(super) fn timer_command(timer_id: TimerId, fire_at: DateTime<Utc>) -> Command {
    Command::StartTimer {
        key: CorrelationKey::Timer(timer_id),
        fire_at,
    }
}

/// Deterministic workflow-visible "now": the timestamp of the last event in
/// the run segment the calling context resolved against.
pub(super) fn recorded_now(context: &NifContext) -> DateTime<Utc> {
    context
        .last_recorded_at()
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

/// The recorded terminal for `timer_id` in this run's segment, if any.
///
/// `Some(true)` means `TimerFired`, `Some(false)` means `TimerCancelled`,
/// `None` means the timer is still pending.
pub(super) fn timer_terminal_recorded(context: &NifContext, timer_id: &TimerId) -> Option<bool> {
    context
        .history()
        .iter()
        .rev()
        .find_map(|event| match event {
            // A TimerStarted seen FIRST in reverse means the timer was
            // (re)armed after any earlier terminal — reopen's re-arm marker
            // (#222) — so the timer is pending again.
            Event::TimerStarted {
                timer_id: recorded, ..
            } if recorded == timer_id => Some(None),
            Event::TimerFired {
                timer_id: recorded, ..
            } if recorded == timer_id => Some(Some(true)),
            Event::TimerCancelled {
                timer_id: recorded,
                cause,
                ..
            } if recorded == timer_id => Some(match cause {
                // Workflow-visible cancellation: the await observes it.
                aion_core::TimerCancelCause::WorkflowIntent => Some(false),
                // Cancel-teardown bookkeeping is never workflow-visible: the
                // await stays pending (reopen re-arms or the run stays dead).
                aion_core::TimerCancelCause::CancelTeardown => None,
            }),
            _ => None,
        })
        .flatten()
}

pub(super) fn add_duration(
    recorded_now: DateTime<Utc>,
    duration: Duration,
) -> Result<DateTime<Utc>, NifTimerError> {
    let chrono_duration = chrono::Duration::from_std(duration)
        .map_err(|_| NifTimerError::Argument("duration is out of range".to_owned()))?;
    recorded_now
        .checked_add_signed(chrono_duration)
        .ok_or_else(|| NifTimerError::Argument("timer fire_at overflowed".to_owned()))
}

pub(super) fn record_started(
    context: &NifContext,
    recorded_at: DateTime<Utc>,
    timer_id: TimerId,
    fire_at: DateTime<Utc>,
) -> Result<(), NifTimerError> {
    context
        .block_on_recorder(|recorder| {
            Box::pin(async move {
                recorder
                    .record_timer_started(recorded_at, timer_id, fire_at)
                    .await
            })
        })
        .map_err(Into::into)
}

#[derive(Clone, Copy)]
enum TimerKind {
    Anonymous {
        duration: Duration,
        recorded_now: DateTime<Utc>,
    },
    Named,
}

pub(super) fn schedule_sleep_timer(
    state: &EngineNifState,
    context: &NifContext,
    duration: Duration,
    recorded_now: DateTime<Utc>,
    timer_id: &TimerId,
    fire_at: DateTime<Utc>,
) -> Result<(), NifTimerError> {
    let kind = TimerKind::Anonymous {
        duration,
        recorded_now,
    };
    let scheduled = schedule_timer(state, context, timer_id.clone(), fire_at, kind)?;
    if scheduled == *timer_id {
        Ok(())
    } else {
        Err(NifTimerError::Context(
            "AT sleep service returned a mismatched timer id".to_owned(),
        ))
    }
}

fn schedule_timer(
    state: &EngineNifState,
    context: &NifContext,
    timer_id: TimerId,
    fire_at: DateTime<Utc>,
    kind: TimerKind,
) -> Result<TimerId, NifTimerError> {
    let workflow_id = context.workflow_id().clone();
    let bridge = timer_bridge(state)?;
    let service = bridge.service();
    let scheduled = run_blocking(&bridge.tokio_handle, async {
        match kind {
            TimerKind::Anonymous {
                duration,
                recorded_now,
            } => time::sleep(
                &service,
                workflow_id,
                duration,
                recorded_now,
                timer_id.sequence_position().ok_or_else(|| {
                    NifTimerError::Context("sleep timer id is not anonymous".to_owned())
                })?,
            )
            .await
            .map(|sleep| sleep.timer_id)
            .map_err(NifTimerError::from),
            TimerKind::Named => time::start_timer(&service, workflow_id, timer_id.clone(), fire_at)
                .await
                .map(|()| timer_id)
                .map_err(NifTimerError::from),
        }
    })?;
    Ok(scheduled)
}

pub(super) fn cancel_live_timer(
    state: &EngineNifState,
    context: &NifContext,
    timer_id: TimerId,
) -> Result<(), NifTimerError> {
    let workflow_id = context.workflow_id().clone();
    let bridge = timer_bridge(state)?;
    let service = bridge.service();
    run_blocking(&bridge.tokio_handle, async {
        time::cancel_timer(&service, workflow_id, timer_id).await
    })?;
    Ok(())
}

fn ok_result(value: impl Into<String>) -> NifResult {
    NifResult::Ok(value.into())
}

fn error_result(value: impl Into<String>) -> NifResult {
    NifResult::Error(value.into())
}

enum NifResult {
    Ok(String),
    Error(String),
    /// Park the calling process via `request_suspend`; a mailbox wake
    /// re-invokes the native to re-run its two-phase resolution.
    Suspend,
}

#[derive(thiserror::Error, Debug)]
pub(super) enum NifTimerError {
    #[error("argument:{0}")]
    Argument(String),
    #[error("context:{0}")]
    Context(String),
    #[error("durability:{0}")]
    Durability(#[from] DurabilityError),
    #[error("timer:{0}")]
    Timer(#[from] crate::time::TimerServiceError),
    #[error("sleep:{0}")]
    Sleep(#[from] crate::time::SleepTimerError),
}

impl From<NifContextError> for NifTimerError {
    fn from(error: NifContextError) -> Self {
        match error {
            NifContextError::Durability(error) => Self::Durability(error),
            other => Self::Context(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{Event, EventEnvelope, Payload, RunId, TimerId, WorkflowId, WorkflowStatus};
    use aion_package::ContentHash;
    use aion_store::{EventStore, InMemoryStore, WriteToken};
    use chrono::{DateTime, Utc};
    use serde_json::json;

    use super::{NifResult, park_sleep};
    use crate::durability::Recorder;
    use crate::registry::{
        CompletionNotifier, HandleResidency, Registry, WorkflowHandle, WorkflowHandleParts,
    };
    use crate::runtime::nif_state::EngineNifState;
    use crate::runtime::nif_timeout::TimeoutScope;
    use crate::runtime::{RuntimeConfig, RuntimeHandle, SignalDeliveryConfig};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// One registered sleeper mid-sleep (`TimerStarted` recorded, no
    /// terminal), with the timer bridge installed and an expired replayed
    /// scope on the pid.
    struct SleeperHarness {
        state: Arc<EngineNifState>,
        store: Arc<dyn EventStore>,
        handle: WorkflowHandle,
        runtime: Arc<RuntimeHandle>,
        workflow_id: WorkflowId,
        pid: u64,
        fire_at: DateTime<Utc>,
    }

    impl SleeperHarness {
        async fn mid_sleep(
            pid: u64,
            sleep_timer: &TimerId,
        ) -> Result<Self, Box<dyn std::error::Error>> {
            let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
            let registry = Arc::new(Registry::default());
            let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
            let workflow_id = WorkflowId::new_v4();
            let run_id = RunId::new_v4();
            let fire_at = Utc::now();
            let envelope = |seq: u64| EventEnvelope {
                seq,
                recorded_at: Utc::now(),
                workflow_id: workflow_id.clone(),
            };
            let events = vec![
                Event::WorkflowStarted {
                    envelope: envelope(1),
                    workflow_type: "sleeper".to_owned(),
                    input: Payload::from_json(&json!({}))?,
                    run_id: run_id.clone(),
                    parent_run_id: None,
                    package_version: aion_core::PackageVersion::new("a".repeat(64)),
                },
                Event::TimerStarted {
                    envelope: envelope(2),
                    timer_id: sleep_timer.clone(),
                    fire_at,
                },
            ];
            store
                .append(WriteToken::recorder(), &workflow_id, &events, 0)
                .await?;
            let recorder = Recorder::resume_at(workflow_id.clone(), Arc::clone(&store), 2);
            let handle = WorkflowHandle::new(WorkflowHandleParts {
                workflow_id: workflow_id.clone(),
                run_id: run_id.clone(),
                pid,
                workflow_type: "sleeper".to_owned(),
                namespace: String::from("default"),
                loaded_version: ContentHash::from_bytes([6; 32]),
                cached_status: WorkflowStatus::Running,
                residency: HandleResidency::Resident,
                recorder,
                completion: CompletionNotifier::new(),
            });
            registry.insert((workflow_id.clone(), run_id), handle.clone())?;
            let state = Arc::new(EngineNifState::default());
            crate::runtime::nif_timer_bridge::install_timer_nif_bridge(
                &state,
                registry,
                Arc::clone(&store),
                tokio::runtime::Handle::current(),
                SignalDeliveryConfig::default(),
            );
            state
                .timeout_scopes
                .insert(1, TimeoutScope::replayed_for_test(pid, true));
            state.timeout_scope_stacks.insert(pid, vec![1]);
            Ok(Self {
                state,
                store,
                handle,
                runtime,
                workflow_id,
                pid,
                fire_at,
            })
        }

        /// Resolution snapshot of the current store state.
        fn snapshot(&self) -> Result<super::NifContext, super::NifTimerError> {
            tokio::task::block_in_place(|| super::build_context_for_pid(&self.state, self.pid))
        }

        fn park(
            &self,
            context: &super::NifContext,
            sleep_timer: &TimerId,
        ) -> Result<NifResult, super::NifTimerError> {
            tokio::task::block_in_place(|| {
                park_sleep(
                    &self.state,
                    context,
                    self.pid,
                    sleep_timer.clone(),
                    self.fire_at,
                )
            })
        }

        fn shutdown(self) -> TestResult {
            self.runtime.shutdown()?;
            Ok(())
        }
    }

    /// N-3: `park_sleep`'s expired-scope abort must not assume the cancel
    /// won. Race modeled: the sleep's resolution snapshot has only
    /// `TimerStarted`; the timer service records `TimerFired` before the
    /// abort's cancel runs, making that cancel a silent no-op
    /// (first-recorded-wins). Before the fix the live path returned
    /// `cancelled` unconditionally while replay read the recorded
    /// `TimerFired` and returned `fired` — opposite results. After the fix
    /// the settled branch is re-read from the recorded terminal: both
    /// return `fired`, and no `TimerCancelled` is ever appended.
    #[tokio::test(flavor = "multi_thread")]
    async fn expired_scope_cancel_losing_to_a_recorded_fire_returns_fired() -> TestResult {
        let sleep_timer = TimerId::anonymous(3);
        let harness = SleeperHarness::mid_sleep(521, &sleep_timer).await?;

        // Resolution snapshot: TimerStarted only.
        let context = harness.snapshot()?;

        // The fire lands durably after the snapshot — the race window.
        {
            let recorder = harness.handle.recorder();
            let mut recorder = recorder.lock().await;
            recorder
                .record_timer_fired(Utc::now(), sleep_timer.clone())
                .await?;
        }

        match harness.park(&context, &sleep_timer)? {
            NifResult::Ok(value) => assert_eq!(value, "fired"),
            NifResult::Error(message) => {
                return Err(format!(
                    "live sleep answered `{message}` where replay reads the recorded \
                     TimerFired and answers `fired` (N-3)"
                )
                .into());
            }
            NifResult::Suspend => return Err("the expired-scope abort must settle".into()),
        }

        // Exactly one terminal exists: the fire. The losing cancel appended
        // nothing, so replay reads the same single terminal.
        let history = harness.store.read_history(&harness.workflow_id).await?;
        let fired = history
            .iter()
            .filter(
                |event| matches!(event, Event::TimerFired { timer_id, .. } if timer_id == &sleep_timer),
            )
            .count();
        let cancelled = history
            .iter()
            .filter(|event| {
                matches!(event, Event::TimerCancelled { timer_id, .. } if timer_id == &sleep_timer)
            })
            .count();
        assert_eq!((fired, cancelled), (1, 0));

        // Replay equivalence: a fresh snapshot over the settled history
        // resolves the same terminal.
        let replayed = harness.snapshot()?;
        assert_eq!(
            super::timer_terminal_recorded(&replayed, &sleep_timer),
            Some(true),
            "replay reads TimerFired for this sleep"
        );
        assert!(harness.state.pending_awaits.get(&harness.pid).is_none());
        harness.shutdown()
    }

    /// An author-supplied timer name under the reserved `deadline:` prefix is
    /// refused at the single decode choke point, so workflow code can never
    /// mint (or address) a per-run workflow deadline.
    #[test]
    fn author_timer_id_refuses_reserved_deadline_prefix() -> TestResult {
        match super::author_timer_id("start_timer", "deadline:some-run") {
            Err(super::NifTimerError::Argument(message)) => {
                assert!(message.contains("reserved"), "message: {message}");
                assert!(message.contains("deadline:"), "message: {message}");
                Ok(())
            }
            other => Err(format!("expected reserved-prefix refusal, got {other:?}").into()),
        }
    }

    /// The schedule coordinator's `schedule:` prefix is equally reserved: it was
    /// author-mintable before this guard (decode did no prefix check), so it is
    /// refused here too.
    #[test]
    fn author_timer_id_refuses_reserved_schedule_prefix() -> TestResult {
        match super::author_timer_id("start_timer", "schedule:daily") {
            Err(super::NifTimerError::Argument(message)) => {
                assert!(message.contains("schedule:"), "message: {message}");
                Ok(())
            }
            other => Err(format!("expected reserved-prefix refusal, got {other:?}").into()),
        }
    }

    /// An ordinary author timer name is accepted unchanged.
    #[test]
    fn author_timer_id_accepts_ordinary_name() -> TestResult {
        let timer_id = super::author_timer_id("start_timer", "review-deadline")?;
        assert_eq!(timer_id.name(), Some("review-deadline"));
        Ok(())
    }

    /// The deterministic abort: no racing fire — the cancel wins, records
    /// `TimerCancelled`, and the sleep answers `cancelled` on live and
    /// replay alike.
    #[tokio::test(flavor = "multi_thread")]
    async fn expired_scope_cancel_winning_returns_cancelled() -> TestResult {
        let sleep_timer = TimerId::anonymous(4);
        let harness = SleeperHarness::mid_sleep(522, &sleep_timer).await?;
        let context = harness.snapshot()?;

        match harness.park(&context, &sleep_timer)? {
            NifResult::Error(message) => assert_eq!(message, "cancelled"),
            NifResult::Ok(value) => return Err(format!("unexpected success: {value}").into()),
            NifResult::Suspend => return Err("the expired-scope abort must settle".into()),
        }
        let history = harness.store.read_history(&harness.workflow_id).await?;
        assert!(history.iter().any(|event| {
            matches!(event, Event::TimerCancelled { timer_id, .. } if timer_id == &sleep_timer)
        }));
        harness.shutdown()
    }
}
