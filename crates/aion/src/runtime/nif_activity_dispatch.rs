//! Two-phase activity dispatch NIFs.

use std::sync::Arc;

use crate::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use crate::durability::{Command, CorrelationKey, Recorder, ResolveOutcome};
use crate::runtime::nif_activity::{
    context_error_term, correlation_id, decode_string_arg, error_result_term, json_payload,
    labels_from_config, ok_result_term, record_started, runtime_context,
};
use crate::runtime::nif_context::NifContext;
use aion_core::ActivityId;
use beamr::native::ProcessContext;
use beamr::term::Term;

/// NIF backing `aion_flow_ffi:dispatch_activity/3`.
pub(super) fn dispatch_activity_impl(
    args: &[Term],
    ctx: &mut ProcessContext,
) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    let Ok((name, input, config)) = decode_dispatch_args(args) else {
        return Ok(error_result_term(
            ctx,
            &format!(
                "dispatch_activity: expected 3 arguments, got {}",
                args.len()
            ),
        )
        .unwrap_or(Term::NIL));
    };
    // Defense: an in-VM selection must cross the arity-4 wire that carries the
    // runner thunk. Refused BEFORE any ordinal allocation or resolve, so
    // nothing is recorded.
    if super::nif_activity::config_tier(&config).as_deref() == Some(super::nif_activity::IN_VM_TIER)
    {
        return Ok(error_result_term(
            ctx,
            "dispatch_activity: tier in_vm cannot cross the remote dispatch wire — \
             in-VM dispatch requires dispatch_activity_in_vm/4 carrying the runner thunk",
        )
        .unwrap_or(Term::NIL));
    }
    let Some(pid) = ctx.pid() else {
        return Ok(
            error_result_term(ctx, "dispatch_activity: missing calling process pid")
                .unwrap_or(Term::NIL),
        );
    };
    let state = match super::nif_state::engine_nif_state(ctx) {
        Ok(state) => state,
        Err(error) => return Ok(error_result_term(ctx, &error).unwrap_or(Term::NIL)),
    };
    // dispatch_activity records `ActivityScheduled`; a query handler must
    // stay read-only.
    if let Err(error) =
        super::nif_query_pump::ensure_not_servicing_query(&state, pid, "dispatch_activity")
    {
        return Ok(error_result_term(ctx, &error).unwrap_or(Term::NIL));
    }
    let runtime = match runtime_context(&state) {
        Ok(runtime) => runtime,
        Err(error) => return Ok(context_error_term(ctx, &error)),
    };
    let context = match NifContext::new(
        pid,
        runtime.registry.as_ref(),
        runtime.tokio_handle.clone(),
        runtime.runtime.signal_delivery(),
    ) {
        Ok(context) => context,
        Err(error) => return Ok(context_error_term(ctx, &error)),
    };
    let dispatcher = state.activity_dispatcher();
    dispatch_activity_with_context(
        ctx,
        context,
        dispatcher,
        runtime.runtime,
        &runtime.tokio_handle,
        ActivityCall {
            name,
            input,
            config,
            attempt: FIRST_DELIVERY_ATTEMPT,
        },
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
        return Ok(error_result_term(
            ctx,
            &format!(
                "await_activity_result: expected 1 argument, got {}",
                args.len()
            ),
        )
        .unwrap_or(Term::NIL));
    }
    let correlation = match decode_string_arg(args[0]) {
        Ok(value) => value,
        Err(error) => {
            return Ok(
                error_result_term(ctx, &format!("await_activity_result id: {error}"))
                    .unwrap_or(Term::NIL),
            );
        }
    };
    let Some(pid) = ctx.pid() else {
        return Ok(
            error_result_term(ctx, "await_activity_result: missing calling process pid")
                .unwrap_or(Term::NIL),
        );
    };
    let state = match super::nif_state::engine_nif_state(ctx) {
        Ok(state) => state,
        Err(error) => return Ok(error_result_term(ctx, &error).unwrap_or(Term::NIL)),
    };
    let runtime = match runtime_context(&state) {
        Ok(runtime) => runtime,
        Err(error) => return Ok(context_error_term(ctx, &error)),
    };
    let context = match NifContext::new(
        pid,
        runtime.registry.as_ref(),
        runtime.tokio_handle,
        runtime.runtime.signal_delivery(),
    ) {
        Ok(context) => context,
        Err(error) => return Ok(context_error_term(ctx, &error)),
    };
    await_activity_result_with_context(&state, context, &runtime.runtime, ctx, &correlation)
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

/// First delivery: a dispatch for an ordinal with no recorded attempt trail
/// is attempt 1.
///
/// The remote-tier retry loop ([`spawn_completion_task`], #197) re-dispatches
/// with the incremented attempt when the SDK-declared retry policy (decoded
/// from the dispatch `config` JSON by [`super::nif_activity_retry`]) has
/// budget left, and a live re-dispatch after a crash continues the recorded
/// trail via [`super::nif_activity_retry::next_delivery_attempt`]. This is
/// the single documented producer-side constant; no consumer guesses an
/// attempt.
///
/// In-VM retry-seam constraint: unlike remote activities, an in-VM retry must
/// be driven from a seam that HOLDS THE RUNNER — the dispatch NIF itself
/// (replay's reopen path re-supplies the thunk on every re-execution of
/// workflow code) or an SDK-level loop. The remote retry loop deliberately
/// does not cover the in-VM tier, whose dispatches stay single-attempt.
pub(super) const FIRST_DELIVERY_ATTEMPT: u32 = 1;

/// Grouped parameters for the activity being dispatched.
///
/// Shared with the `collect_*` fan-out natives, which dispatch N of these
/// through the same completion-task machinery.
pub(super) struct ActivityCall {
    pub(super) name: String,
    pub(super) input: String,
    pub(super) config: String,
    /// One-based delivery attempt stamped onto the dispatch (and from there
    /// onto the worker wire). See [`FIRST_DELIVERY_ATTEMPT`].
    pub(super) attempt: u32,
}

fn dispatch_activity_with_context(
    ctx: &mut ProcessContext,
    mut context: NifContext,
    dispatcher: Option<Arc<dyn ActivityDispatcher>>,
    runtime: Arc<crate::RuntimeHandle>,
    tokio_handle: &tokio::runtime::Handle,
    call: ActivityCall,
) -> Result<Term, Term> {
    let input_payload = json_payload(ctx, &call.input, "dispatch_activity", "input")?;
    let ordinal = context.next_activity_ordinal();
    let key = CorrelationKey::Activity(ordinal);
    let activity_id = ActivityId::from_sequence_position(ordinal);
    let correlation = correlation_id(ordinal);
    let namespace = context.workflow_handle().namespace().to_owned();
    match context
        .resolve_command(Command::RunActivity {
            key,
            activity_type: call.name.clone(),
            input: input_payload.clone(),
        })
        .map_err(|error| context_error_term(ctx, &error))?
    {
        ResolveOutcome::Recorded(_) => {
            Ok(ok_result_term(ctx, correlation.as_bytes()).unwrap_or(Term::NIL))
        }
        ResolveOutcome::ResumeLive => {
            let Some(dispatcher) = dispatcher else {
                return Ok(error_result_term(
                    ctx,
                    "no activity dispatcher configured — set one via EngineBuilder::activity_dispatcher",
                )
                .unwrap_or(Term::NIL));
            };
            // NSTQ-4 (+#144): resolve the dispatch's task queue once at this
            // schedule seam (activity override > workflow declared default >
            // the workflow's RECORDED start-time queue > the named default),
            // then stamp the same value onto BOTH the recorded
            // `ActivityScheduled` and the live dispatch so history and routing
            // never diverge. The start-time queue is read from recorded history,
            // so replay re-resolves identically.
            let start_time_task_queue = context.start_time_task_queue();
            let task_queue = super::nif_activity::resolve_task_queue(
                &call.config,
                start_time_task_queue.as_deref(),
            );
            // NODE-4: resolve the OPTIONAL node affinity once at the same seam
            // (activity pin, else None — no workflow default), and stamp the same
            // value onto BOTH the recorded `ActivityScheduled` and the live
            // dispatch so history and routing never diverge.
            let node = super::nif_activity::resolve_node(&call.config);
            // #197: a live re-dispatch continues the ordinal's recorded attempt
            // trail instead of restarting it — a crash-recovery resume after a
            // dangling retryable failure keeps `(workflow, activity, attempt)` a
            // stable identity. A fresh ordinal resolves to `call.attempt`
            // (`FIRST_DELIVERY_ATTEMPT`) exactly as before.
            let attempt =
                super::nif_activity_retry::next_delivery_attempt(context.history(), &activity_id)
                    .max(call.attempt);
            record_started(
                ctx,
                &context,
                activity_id.clone(),
                super::nif_activity::ScheduledActivity {
                    activity_type: call.name.clone(),
                    input: input_payload,
                    task_queue: task_queue.clone(),
                    node: node.clone(),
                    // NOI-0: stamp the SAME one-based attempt onto the recorded `ActivityStarted`
                    // that is stamped onto the live `ActivityDispatch` below, so history and the
                    // worker wire agree.
                    attempt,
                },
            )?;
            let labels = labels_from_config(&call.config);
            let request = ActivityDispatch {
                namespace,
                task_queue,
                node,
                workflow_id: context.workflow_id().clone(),
                activity_id,
                name: call.name,
                input: call.input,
                config: call.config,
                attempt,
                labels,
            };
            spawn_completion_task(
                tokio_handle,
                runtime,
                dispatcher,
                RetryRecorderSeam {
                    recorder: context.recorder(),
                    run_id: context.workflow_handle().run_id().clone(),
                },
                context.pid(),
                correlation.clone(),
                request,
            );
            Ok(ok_result_term(ctx, correlation.as_bytes()).unwrap_or(Term::NIL))
        }
    }
}

/// Spawn the completion task for one dispatched activity.
///
/// The task drives the dispatch to its FINAL outcome before waking the
/// workflow: a retryable-class failure (`retryable:` reason prefix — the
/// string form of the wire's structured `ActivityErrorKind`, see
/// [`super::nif_activity_retry`]) with budget left under the SDK-declared
/// retry policy is recorded durably as a non-terminal `ActivityFailed`
/// (kind `Retryable`), backed off, and re-dispatched with the SAME ordinal
/// and routing at the incremented attempt. Non-retryable failures, absent
/// policies (`"retry": null` — the SDK's run-exactly-once contract), and an
/// exhausted budget deliver to the workflow exactly as before, with the last
/// reason verbatim.
///
/// Every durable retry record is guarded against the settle races the
/// workflow thread can win mid-loop (a `with_timeout` expiry recording the
/// ordinal's terminal, a workflow terminal): the guard re-reads history under
/// the recorder lock and aborts the loop once the decision was made elsewhere.
/// The backoff sleep itself is task-local, not a durable timer: an engine
/// crash mid-backoff recovers through replay, whose dangling retryable
/// failure re-dispatches the activity live at the next attempt.
pub(super) fn spawn_completion_task(
    tokio_handle: &tokio::runtime::Handle,
    runtime: Arc<crate::RuntimeHandle>,
    dispatcher: Arc<dyn ActivityDispatcher>,
    seam: RetryRecorderSeam,
    workflow_pid: u64,
    correlation_id: String,
    request: ActivityDispatch,
) {
    let future = async move {
        let outcome = dispatch_with_retries(&dispatcher, &seam, &request).await;
        let attempt = outcome.attempt;
        match outcome.terminal {
            RetryLoopTerminal::Completed(payload) => {
                if let Err(error) = runtime.deliver_activity_completion_message_with_attempt(
                    workflow_pid,
                    &correlation_id,
                    payload,
                    Some(attempt),
                ) {
                    tracing::warn!(%error, workflow_pid, correlation_id, "activity completion delivery failed");
                }
            }
            RetryLoopTerminal::Failed(reason) => {
                if let Err(error) = runtime.deliver_activity_failure_message_with_attempt(
                    workflow_pid,
                    &correlation_id,
                    reason,
                    Some(attempt),
                ) {
                    tracing::warn!(%error, workflow_pid, correlation_id, "activity failure delivery failed");
                }
            }
            RetryLoopTerminal::SettledElsewhere => {
                // The awaited ordinal (or the whole workflow) reached a
                // recorded terminal while the loop ran — deliver nothing; the
                // workflow already took that branch.
                tracing::debug!(
                    workflow_id = %request.workflow_id,
                    activity_id = %request.activity_id,
                    attempt,
                    "activity retry loop stopped: the activity settled through another path"
                );
            }
            RetryLoopTerminal::Parked => {
                // The server parked this dispatch for restart recovery
                // (graceful drain, #207): record nothing, deliver nothing. The
                // durable log ends at the dangling scheduled/started trail —
                // byte-equivalent to a kill -9 — so post-restart replay
                // re-dispatches the activity live (cursor Exhausted →
                // ResumeLive), exactly the SettledElsewhere stand-down shape.
                tracing::debug!(
                    workflow_id = %request.workflow_id,
                    activity_id = %request.activity_id,
                    attempt,
                    "activity dispatch parked for restart recovery; retry loop stood down"
                );
            }
        }
    };
    tokio_handle.spawn(future);
}

/// The durable seam one completion task records retry attempts through: the
/// workflow's single-writer recorder plus the run the dispatch belongs to
/// (settlement is a per-run question — see [`record_retry_event`]).
pub(super) struct RetryRecorderSeam {
    /// The workflow's single-writer recorder, shared with the NIF contexts.
    pub(super) recorder: Arc<tokio::sync::Mutex<Recorder>>,
    /// The run this dispatch was issued by.
    pub(super) run_id: aion_core::RunId,
}

/// The retry loop's final disposition, carrying the attempt that produced it.
struct RetryLoopOutcome {
    attempt: u32,
    terminal: RetryLoopTerminal,
}

enum RetryLoopTerminal {
    /// The encoded output of the successful attempt.
    Completed(String),
    /// The last failure reason, verbatim (prefix included).
    Failed(String),
    /// A terminal for this ordinal/workflow was recorded by another path
    /// mid-loop; nothing may be delivered or recorded for it anymore.
    SettledElsewhere,
    /// The server parked the dispatch for restart recovery during a graceful
    /// drain (#207): nothing may be delivered or recorded — the workflow stays
    /// suspended and post-restart replay re-dispatches the dangling ordinal.
    Parked,
}

/// Classify a failed dispatch BEFORE any durable retry record: the parked
/// sentinel stands the loop down (park beats retry, #207 — nothing recorded,
/// nothing delivered, no budget consumed; restart recovery re-dispatches the
/// dangling ordinal); a non-retryable class, an absent policy (`"retry":
/// null`), or an exhausted budget fails with the reason verbatim. `None`
/// means the loop retries under the policy.
fn failure_stand_down(
    policy: Option<&super::nif_activity_retry::RetryPolicy>,
    reason: &str,
    attempt: u32,
) -> Option<RetryLoopTerminal> {
    use super::nif_activity_retry::{is_parked_reason, is_retryable_reason};

    if is_parked_reason(reason) {
        return Some(RetryLoopTerminal::Parked);
    }
    match policy {
        Some(policy) if is_retryable_reason(reason) && attempt < policy.max_attempts => None,
        _ => Some(RetryLoopTerminal::Failed(reason.to_owned())),
    }
}

/// Drive one activity dispatch to its final outcome under the SDK-declared
/// retry policy carried in the dispatch config (#197).
async fn dispatch_with_retries(
    dispatcher: &Arc<dyn ActivityDispatcher>,
    seam: &RetryRecorderSeam,
    request: &ActivityDispatch,
) -> RetryLoopOutcome {
    use super::nif_activity_retry::retry_policy_from_config;

    let policy = retry_policy_from_config(&request.config);
    let mut attempt = request.attempt;
    loop {
        let mut delivery = request.clone();
        delivery.attempt = attempt;
        let reason = match Arc::clone(dispatcher).dispatch_async(delivery).await {
            Ok(payload) => {
                return RetryLoopOutcome {
                    attempt,
                    terminal: RetryLoopTerminal::Completed(payload),
                };
            }
            Err(reason) => reason,
        };
        if let Some(terminal) = failure_stand_down(policy.as_ref(), &reason, attempt) {
            return RetryLoopOutcome { attempt, terminal };
        }
        // The stand-down above returns `Failed` whenever no policy is present,
        // so a `None` here is structurally unreachable — kept as the honest
        // failure terminal rather than an unwrap.
        let Some(policy) = policy.as_ref() else {
            return RetryLoopOutcome {
                attempt,
                terminal: RetryLoopTerminal::Failed(reason),
            };
        };
        // Record the failed attempt durably as a NON-terminal (Retryable)
        // `ActivityFailed` — the observable retry record the history cursor
        // walks past to the eventual terminal. Recording failures abort the
        // loop into an honest terminal failure: an unrecorded retry is a
        // silent retry.
        match record_retry_event(
            seam,
            request,
            RetryRecord::AttemptFailed {
                attempt,
                reason: reason.clone(),
            },
        )
        .await
        {
            RetryRecordOutcome::Recorded => {}
            RetryRecordOutcome::Settled => {
                return RetryLoopOutcome {
                    attempt,
                    terminal: RetryLoopTerminal::SettledElsewhere,
                };
            }
            RetryRecordOutcome::RecordFailed(record_error) => {
                tracing::warn!(
                    workflow_id = %request.workflow_id,
                    activity_id = %request.activity_id,
                    attempt,
                    error = %record_error,
                    "failed to record a retryable activity failure; failing the activity instead \
                     of retrying unrecorded"
                );
                return RetryLoopOutcome {
                    attempt,
                    terminal: RetryLoopTerminal::Failed(reason),
                };
            }
        }
        let delay = policy.backoff.delay_after(attempt);
        tracing::warn!(
            workflow_id = %request.workflow_id,
            activity_id = %request.activity_id,
            activity_type = %request.name,
            attempt,
            max_attempts = policy.max_attempts,
            retry_in_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            reason = %reason,
            "activity attempt failed with a retryable error; re-dispatching"
        );
        tokio::time::sleep(delay).await;
        attempt += 1;
        // Record the retry delivery's `ActivityStarted` before it goes on the
        // wire, so history and the worker wire agree on the attempt (NOI-0) —
        // re-guarded, because the backoff sleep is a settle-race window.
        match record_retry_event(seam, request, RetryRecord::AttemptStarted { attempt }).await {
            RetryRecordOutcome::Recorded => {}
            RetryRecordOutcome::Settled => {
                return RetryLoopOutcome {
                    attempt,
                    terminal: RetryLoopTerminal::SettledElsewhere,
                };
            }
            RetryRecordOutcome::RecordFailed(record_error) => {
                tracing::warn!(
                    workflow_id = %request.workflow_id,
                    activity_id = %request.activity_id,
                    attempt,
                    error = %record_error,
                    "failed to record a retry attempt start; failing the activity instead of \
                     dispatching unrecorded"
                );
                return RetryLoopOutcome {
                    attempt,
                    terminal: RetryLoopTerminal::Failed(reason),
                };
            }
        }
    }
}

/// One durable retry record the loop appends between attempts.
enum RetryRecord {
    /// The just-failed attempt's non-terminal `ActivityFailed`.
    AttemptFailed { attempt: u32, reason: String },
    /// The next delivery's `ActivityStarted`.
    AttemptStarted { attempt: u32 },
}

enum RetryRecordOutcome {
    Recorded,
    /// The ordinal (or workflow) already has a recorded terminal; the loop
    /// must stop without recording or delivering anything further.
    Settled,
    RecordFailed(crate::durability::DurabilityError),
}

/// Append one retry record under the recorder lock, re-checking settlement
/// first so the append can never land after a terminal recorded by the
/// workflow thread (`with_timeout` expiry, workflow terminal).
async fn record_retry_event(
    seam: &RetryRecorderSeam,
    request: &ActivityDispatch,
    record: RetryRecord,
) -> RetryRecordOutcome {
    let mut recorder = seam.recorder.lock().await;
    let history = match recorder.read_history().await {
        Ok(history) => history,
        Err(error) => return RetryRecordOutcome::RecordFailed(error),
    };
    // Settlement is a per-run question: scope to the current run's segment so
    // a prior run's terminal (continue-as-new) never aborts this run's loop.
    let history = match crate::durability::current_run_segment(history, &seam.run_id) {
        Ok(history) => history,
        Err(error) => return RetryRecordOutcome::RecordFailed(error),
    };
    if super::nif_activity_retry::activity_settled(&history, &request.activity_id) {
        return RetryRecordOutcome::Settled;
    }
    let append_result = match record {
        RetryRecord::AttemptFailed { attempt, reason } => {
            recorder
                .record_activity_failed(
                    chrono::Utc::now(),
                    request.activity_id.clone(),
                    aion_core::ActivityError {
                        kind: aion_core::ActivityErrorKind::Retryable,
                        message: reason,
                        details: None,
                    },
                    attempt,
                )
                .await
        }
        RetryRecord::AttemptStarted { attempt } => {
            recorder
                .record_activity_started(chrono::Utc::now(), request.activity_id.clone(), attempt)
                .await
        }
    };
    match append_result {
        Ok(()) => RetryRecordOutcome::Recorded,
        Err(error) => RetryRecordOutcome::RecordFailed(error),
    }
}

use super::nif_activity_await::await_activity_result_with_context;
#[cfg(test)]
pub(super) use super::nif_activity_await::{ActivityAwaitStep, await_activity_step};

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{
        ActivityId, ContentType, Event, EventEnvelope, Payload, RunId, WorkflowId, WorkflowStatus,
    };
    use aion_package::ContentHash;
    use aion_store::{EventStore, WriteToken};
    use serde_json::json;

    use super::{ActivityAwaitStep, await_activity_step, spawn_completion_task};
    use crate::activity::bridge::{ActivityDispatch, ActivityDispatcher};
    use crate::durability::Recorder;
    use crate::error::EngineError;
    use crate::registry::{
        CompletionNotifier, HandleResidency, Registry, WorkflowHandle, WorkflowHandleParts,
    };
    use crate::runtime::nif_state::EngineNifState;
    use crate::runtime::nif_test_stores::StaleReadStore;
    use crate::runtime::nif_timeout::TimeoutScope;
    use crate::runtime::{RuntimeConfig, RuntimeHandle, SignalDeliveryConfig};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Everything one `await_activity_step` determinism test needs over a
    /// synthesized history.
    struct AwaitHarness {
        state: Arc<EngineNifState>,
        registry: Arc<Registry>,
        runtime: Arc<RuntimeHandle>,
        store: Arc<dyn EventStore>,
        workflow_id: WorkflowId,
        pid: u64,
    }

    impl AwaitHarness {
        /// Build a fresh engine epoch (registry, handle, runtime) over an
        /// existing seeded store — the unit-level analogue of an engine
        /// restart before replay.
        async fn over_store(
            store: Arc<dyn EventStore>,
            workflow_id: WorkflowId,
            run_id: RunId,
        ) -> Result<Self, Box<dyn std::error::Error>> {
            let head = u64::try_from(store.read_history(&workflow_id).await?.len())?;
            let registry = Arc::new(Registry::default());
            let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
            let pid = runtime.spawn_test_process()?;
            let recorder = Recorder::resume_at(workflow_id.clone(), Arc::clone(&store), head);
            let handle = WorkflowHandle::new(WorkflowHandleParts {
                workflow_id: workflow_id.clone(),
                run_id: run_id.clone(),
                pid,
                workflow_type: "awaiter".to_owned(),
                namespace: String::from("default"),
                loaded_version: ContentHash::from_bytes([7; 32]),
                cached_status: WorkflowStatus::Running,
                residency: HandleResidency::Resident,
                recorder,
                completion: CompletionNotifier::new(),
            });
            registry.insert((workflow_id.clone(), run_id), handle)?;
            Ok(Self {
                state: Arc::new(EngineNifState::default()),
                registry,
                runtime,
                store,
                workflow_id,
                pid,
            })
        }

        /// One production-shaped pass: a fresh `NifContext` (one history
        /// read — the resolution snapshot) resolving the ordinal-0 await.
        fn step(&self) -> Result<ActivityAwaitStep, String> {
            self.step_typed().map_err(|error| error.to_string())
        }

        fn step_typed(&self) -> Result<ActivityAwaitStep, EngineError> {
            // Production runs this on a beamr scheduler thread with no
            // ambient Tokio context; block_in_place mirrors that so the
            // step's history reads can block_on the harness runtime.
            tokio::task::block_in_place(|| {
                let mut context = crate::runtime::nif_context::NifContext::new(
                    self.pid,
                    self.registry.as_ref(),
                    tokio::runtime::Handle::current(),
                    SignalDeliveryConfig::default(),
                )
                .map_err(|error| EngineError::Runtime {
                    reason: error.error_reason(),
                })?;
                await_activity_step(
                    &self.state,
                    &mut context,
                    &self.runtime,
                    &ActivityId::from_sequence_position(0),
                    || {},
                )
            })
        }

        /// Arm the per-test timer bridge that backed the OLD fresh-read
        /// expiry path (`expired_scope_message` → `build_context_for_pid`);
        /// installing it proves the stale-snapshot test fails if a fresh
        /// read is reintroduced, instead of accidentally passing because
        /// the fresh read was unavailable.
        fn install_fresh_read_bridge(&self) {
            crate::runtime::nif_timer_bridge::install_timer_nif_bridge(
                &self.state,
                Arc::clone(&self.registry),
                Arc::clone(&self.store),
                tokio::runtime::Handle::current(),
                SignalDeliveryConfig::default(),
            );
        }

        fn arm_live_scope(&self, deadline_ordinal: u64) {
            self.state.timeout_scopes.insert(
                31,
                TimeoutScope::live_for_test(
                    self.pid,
                    aion_core::TimerId::anonymous(deadline_ordinal),
                ),
            );
            self.state.timeout_scope_stacks.insert(self.pid, vec![31]);
        }

        fn arm_replayed_expired_scope(&self, deadline_ordinal: u64) {
            self.state.timeout_scopes.insert(
                1,
                TimeoutScope::replayed_expired_with_deadline_for_test(
                    self.pid,
                    aion_core::TimerId::anonymous(deadline_ordinal),
                ),
            );
            self.state.timeout_scope_stacks.insert(self.pid, vec![1]);
        }

        async fn history_len(&self) -> Result<usize, Box<dyn std::error::Error>> {
            Ok(self.store.read_history(&self.workflow_id).await?.len())
        }

        fn shutdown(self) -> TestResult {
            self.runtime.shutdown()?;
            Ok(())
        }
    }

    fn envelope(workflow_id: &WorkflowId, seq: u64) -> EventEnvelope {
        EventEnvelope {
            seq,
            recorded_at: chrono::Utc::now(),
            workflow_id: workflow_id.clone(),
        }
    }

    /// Seed `WorkflowStarted` + a scheduled/started ordinal-0 activity +
    /// the scope deadline's `TimerFired` (seq 4).
    async fn seed_pending_activity_then_deadline(
        store: &Arc<dyn EventStore>,
        deadline_ordinal: u64,
    ) -> Result<(WorkflowId, RunId), Box<dyn std::error::Error>> {
        let workflow_id = WorkflowId::new_v4();
        let run_id = RunId::new_v4();
        let events = vec![
            Event::WorkflowStarted {
                envelope: envelope(&workflow_id, 1),
                workflow_type: "awaiter".to_owned(),
                input: Payload::from_json(&json!({}))?,
                run_id: run_id.clone(),
                parent_run_id: None,
                package_version: aion_core::PackageVersion::new("a".repeat(64)),
            },
            Event::ActivityScheduled {
                envelope: envelope(&workflow_id, 2),
                activity_id: ActivityId::from_sequence_position(0),
                activity_type: "work".to_owned(),
                input: Payload::new(ContentType::Json, br#""in""#.to_vec()),
                task_queue: String::from("default"),
                node: None,
            },
            Event::ActivityStarted {
                envelope: envelope(&workflow_id, 3),
                activity_id: ActivityId::from_sequence_position(0),
                attempt: 1,
            },
            Event::TimerFired {
                envelope: envelope(&workflow_id, 4),
                timer_id: aion_core::TimerId::anonymous(deadline_ordinal),
            },
        ];
        store
            .append(WriteToken::recorder(), &workflow_id, &events, 0)
            .await?;
        Ok((workflow_id, run_id))
    }

    /// The live expiry decision must be a pure function of the RESOLUTION
    /// snapshot. Race modeled: the await's snapshot (a stale read) lacks
    /// the scope deadline's `TimerFired`, which is recorded by the time any
    /// later read runs. Before the fix `expired_scope_message` re-read the
    /// store, saw the fired deadline, and recorded the durable timeout
    /// failure on the spot — a branch decided from events the resolution
    /// never observed. After the fix the stale-snapshot pass suspends; the
    /// deadline's wake re-enters with a fresh snapshot, records the timeout
    /// failure durably, and a fresh engine epoch returns it verbatim while
    /// appending nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn stale_snapshot_expiry_suspends_then_converges_with_replay() -> TestResult {
        // Stale snapshot = WorkflowStarted + Scheduled + Started: the
        // deadline `TimerFired` (seq 4) is the one event past the window.
        let backing = Arc::new(StaleReadStore::new(3));
        let store: Arc<dyn EventStore> = Arc::clone(&backing) as Arc<dyn EventStore>;
        let (workflow_id, run_id) = seed_pending_activity_then_deadline(&store, 7).await?;
        let harness =
            AwaitHarness::over_store(Arc::clone(&store), workflow_id.clone(), run_id.clone())
                .await?;
        harness.install_fresh_read_bridge();
        backing.set_stale_target(&workflow_id, 1);
        harness.arm_live_scope(7);

        // Pass 1 — stale resolution snapshot (no terminal, no TimerFired):
        // must suspend, never decide the branch from a fresh read; nothing
        // is recorded.
        assert_eq!(
            harness.step(),
            Ok(ActivityAwaitStep::Suspend),
            "a snapshot lacking both events must park, not branch"
        );
        assert_eq!(harness.history_len().await?, 4);

        // Pass 2 — fresh snapshot: the deadline is in the resolution read;
        // the timeout failure is recorded durably and returned.
        assert_eq!(
            harness.step(),
            Ok(ActivityAwaitStep::Failed(
                "timeout:deadline expired".to_owned()
            ))
        );
        let history = harness.store.read_history(&workflow_id).await?;
        assert!(
            matches!(history.last(), Some(Event::ActivityFailed { .. })),
            "the timeout branch must be recorded durably: {history:#?}"
        );
        let history_len = history.len();
        harness.shutdown()?;

        // Fresh engine epoch over the final store (the restart analogue),
        // scope replay-derived expired exactly as `arm_scope` derives it:
        // the recorded failure resolves verbatim, appending nothing.
        let replay = AwaitHarness::over_store(store, workflow_id, run_id).await?;
        replay.arm_replayed_expired_scope(7);
        assert_eq!(
            replay.step(),
            Ok(ActivityAwaitStep::Failed(
                "timeout:deadline expired".to_owned()
            )),
            "replay must take the same branch as the converged live run"
        );
        assert_eq!(replay.history_len().await?, history_len);
        replay.shutdown()
    }

    /// A completion sitting in the runtime maps settles the await — and is
    /// recorded durably — ahead of the scope-expiry branch, even when the
    /// resolution snapshot already contains the fired deadline. The
    /// recorded terminal IS the decision, so a fresh engine epoch resolves
    /// the completion identically (no deadline-vs-terminal seq ordering is
    /// needed for activities: this await records its own terminals, no
    /// third party races them into history).
    #[tokio::test(flavor = "multi_thread")]
    async fn delivered_completion_settles_durably_ahead_of_snapshot_expiry() -> TestResult {
        let backing = Arc::new(StaleReadStore::new(0));
        let store: Arc<dyn EventStore> = Arc::clone(&backing) as Arc<dyn EventStore>;
        let (workflow_id, run_id) = seed_pending_activity_then_deadline(&store, 7).await?;
        let harness =
            AwaitHarness::over_store(Arc::clone(&store), workflow_id.clone(), run_id.clone())
                .await?;
        harness.arm_live_scope(7);
        harness.runtime.deliver_activity_completion_message(
            harness.pid,
            "activity:0",
            r#""r0""#.to_owned(),
        )?;

        assert_eq!(
            harness.step(),
            Ok(ActivityAwaitStep::Completed(br#""r0""#.to_vec()))
        );
        let history = harness.store.read_history(&workflow_id).await?;
        assert!(
            matches!(history.last(), Some(Event::ActivityCompleted { .. })),
            "the completion must be recorded durably: {history:#?}"
        );
        let history_len = history.len();
        harness.shutdown()?;

        let replay = AwaitHarness::over_store(store, workflow_id, run_id).await?;
        replay.arm_replayed_expired_scope(7);
        assert_eq!(
            replay.step(),
            Ok(ActivityAwaitStep::Completed(br#""r0""#.to_vec())),
            "replay must resolve the recorded completion, not re-derive the race"
        );
        assert_eq!(replay.history_len().await?, history_len);
        replay.shutdown()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poisoned_take_fails_typed_and_monitor_drains_retained_state() -> TestResult {
        let backing = Arc::new(StaleReadStore::new(0));
        let store: Arc<dyn EventStore> = Arc::clone(&backing) as Arc<dyn EventStore>;
        let (workflow_id, run_id) = seed_pending_activity_then_deadline(&store, 7).await?;
        let harness = AwaitHarness::over_store(store, workflow_id, run_id).await?;
        let baseline = harness.runtime.activity_delivery_gate_count();
        harness
            .runtime
            .deliver_activity_completion_message_with_attempt(
                harness.pid,
                "activity:0",
                r#""retained""#.to_owned(),
                Some(3),
            )?;
        assert_eq!(harness.runtime.retained_activity_completions(), 1);
        assert_eq!(
            harness.runtime.retained_activity_attempt_count_for_test(),
            1
        );
        harness
            .runtime
            .force_activity_delivery_poison_for_test(harness.pid)?;

        let step = harness.step_typed();
        assert!(matches!(
            step,
            Err(EngineError::ActivityDeliveryPoisoned { process_id })
                if process_id == harness.pid
        ));
        assert_eq!(
            harness.history_len().await?,
            4,
            "poison must neither suspend nor record a fabricated attempt-one completion"
        );

        let (monitor_sender, monitor_receiver) = std::sync::mpsc::channel();
        harness
            .runtime
            .monitor_process_for_test(harness.pid, move |outcome| {
                if monitor_sender.send(outcome).is_err() {
                    tracing::error!("poisoned-take monitor receiver dropped");
                }
            })?;
        harness.runtime.cancel_pid(harness.pid)?;
        let monitored = monitor_receiver.recv_timeout(std::time::Duration::from_secs(10))?;
        assert!(matches!(
            monitored,
            Err(EngineError::ActivityDeliveryPoisoned { process_id })
                if process_id == harness.pid
        ));
        assert_eq!(harness.runtime.retained_activity_completions(), 0);
        assert_eq!(
            harness.runtime.retained_activity_attempt_count_for_test(),
            0
        );
        assert_eq!(harness.runtime.activity_delivery_gate_count(), baseline);
        harness.shutdown()
    }

    /// Synchronous dispatcher that parks its calling thread on a channel
    /// until the test's release task — running on the same Tokio runtime —
    /// frees it.
    struct GatedDispatcher {
        release: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    impl ActivityDispatcher for GatedDispatcher {
        fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
            let receiver = self
                .release
                .lock()
                .map_err(|_| "release lock poisoned".to_owned())?
                .take()
                .ok_or_else(|| "dispatch invoked more than once".to_owned())?;
            receiver
                .recv()
                .map_err(|error| format!("release channel closed: {error}"))?;
            Ok(request.input)
        }
    }

    /// The whole single-worker scenario, run on a watchdog-guarded thread:
    /// dispatch a gated blocking activity, prove the runtime's only executor
    /// thread is still free by releasing the gate from a task spawned on
    /// that same runtime, then observe the delivered completion payload.
    fn blocking_dispatch_scenario() -> Result<Vec<u8>, String> {
        let tokio_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        tokio_runtime.block_on(async {
            let runtime = Arc::new(
                RuntimeHandle::new(RuntimeConfig::new(Some(1)))
                    .map_err(|error| error.to_string())?,
            );
            let pid = runtime
                .spawn_test_process()
                .map_err(|error| error.to_string())?;
            let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
            let dispatcher: Arc<dyn ActivityDispatcher> = Arc::new(GatedDispatcher {
                release: std::sync::Mutex::new(Some(release_rx)),
            });
            let workflow_id = WorkflowId::new_v4();
            let recorder = Arc::new(tokio::sync::Mutex::new(Recorder::new(
                workflow_id.clone(),
                Arc::new(aion_store::InMemoryStore::default()),
            )));
            spawn_completion_task(
                &tokio::runtime::Handle::current(),
                Arc::clone(&runtime),
                dispatcher,
                super::RetryRecorderSeam {
                    recorder,
                    run_id: RunId::new_v4(),
                },
                pid,
                super::correlation_id(0),
                ActivityDispatch {
                    namespace: String::from("default"),
                    task_queue: String::from("default"),
                    node: None,
                    workflow_id,
                    activity_id: ActivityId::from_sequence_position(0),
                    name: "gated".to_owned(),
                    input: r#""r0""#.to_owned(),
                    config: "{}".to_owned(),
                    attempt: super::FIRST_DELIVERY_ATTEMPT,
                    labels: std::collections::BTreeMap::new(),
                },
            );
            // The release runs as a task on this same single-threaded
            // runtime, spawned AFTER the completion task: it can only
            // execute if the blocking dispatch is not occupying the
            // executor thread.
            tokio::spawn(async move { release_tx.send(()) })
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())?;
            let mut payload = None;
            for _ in 0_u32..2_000 {
                match runtime.take_activity_result(pid, 0) {
                    Ok(Some((delivered, _))) => {
                        payload = Some(delivered.bytes().to_vec());
                        break;
                    }
                    Ok(None) => {}
                    Err(error) => return Err(error.to_string()),
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            runtime.shutdown().map_err(|error| error.to_string())?;
            payload.ok_or_else(|| "activity completion was never delivered".to_owned())
        })
    }

    /// Regression (closeout rider a): a blocking `ActivityDispatcher` must
    /// not wedge a single-threaded engine runtime. Before the
    /// `spawn_blocking` routing in `dispatch_async_from_process`, the
    /// completion task polled the synchronous dispatch inline on the
    /// runtime's only worker thread, so the release task never ran and the
    /// dispatch parked forever — stalling every task on the runtime,
    /// queries included. The watchdog bounds that wedge to a clean failure
    /// instead of a hung suite.
    #[test]
    fn blocking_dispatcher_completes_on_single_threaded_runtime() -> TestResult {
        let (verdict_tx, verdict_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || drop(verdict_tx.send(blocking_dispatch_scenario())));
        let payload = verdict_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .map_err(
                |_| "scenario wedged: the blocking dispatch occupied the only executor thread",
            )??;
        assert_eq!(payload, br#""r0""#.to_vec());
        Ok(())
    }

    // ---- #197: retry loop at the dispatch seam ------------------------------

    /// Scripted dispatcher for the retry-loop tests: pops one outcome per
    /// dispatch and records the wire attempt each delivery carried.
    struct ScriptedRetryDispatcher {
        outcomes: std::sync::Mutex<std::collections::VecDeque<Result<String, String>>>,
        attempts: std::sync::Mutex<Vec<u32>>,
    }

    impl ScriptedRetryDispatcher {
        fn new(outcomes: Vec<Result<String, String>>) -> Arc<Self> {
            Arc::new(Self {
                outcomes: std::sync::Mutex::new(outcomes.into_iter().collect()),
                attempts: std::sync::Mutex::new(Vec::new()),
            })
        }

        fn seen_attempts(&self) -> Vec<u32> {
            self.attempts
                .lock()
                .map(|attempts| attempts.clone())
                .unwrap_or_default()
        }
    }

    impl ActivityDispatcher for ScriptedRetryDispatcher {
        fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
            self.attempts
                .lock()
                .map_err(|_| "attempts lock poisoned".to_owned())?
                .push(request.attempt);
            self.outcomes
                .lock()
                .map_err(|_| "outcomes lock poisoned".to_owned())?
                .pop_front()
                .ok_or_else(|| "terminal:script exhausted — unexpected extra dispatch".to_owned())?
        }
    }

    /// Store + recorder + request over a seeded `WorkflowStarted` +
    /// `ActivityScheduled` + `ActivityStarted(attempt 1)` history — exactly
    /// what the dispatch NIF records before spawning the completion task.
    struct RetryLoopHarness {
        store: Arc<dyn EventStore>,
        seam: super::RetryRecorderSeam,
        request: ActivityDispatch,
        workflow_id: WorkflowId,
    }

    impl RetryLoopHarness {
        async fn seeded(config: &str) -> Result<Self, Box<dyn std::error::Error>> {
            let store: Arc<dyn EventStore> = Arc::new(aion_store::InMemoryStore::default());
            let workflow_id = WorkflowId::new_v4();
            let run_id = RunId::new_v4();
            let events = vec![
                Event::WorkflowStarted {
                    envelope: envelope(&workflow_id, 1),
                    workflow_type: "retrier".to_owned(),
                    input: Payload::from_json(&json!({}))?,
                    run_id: run_id.clone(),
                    parent_run_id: None,
                    package_version: aion_core::PackageVersion::new("b".repeat(64)),
                },
                Event::ActivityScheduled {
                    envelope: envelope(&workflow_id, 2),
                    activity_id: ActivityId::from_sequence_position(0),
                    activity_type: "flaky".to_owned(),
                    input: Payload::new(ContentType::Json, br#""in""#.to_vec()),
                    task_queue: String::from("default"),
                    node: None,
                },
                Event::ActivityStarted {
                    envelope: envelope(&workflow_id, 3),
                    activity_id: ActivityId::from_sequence_position(0),
                    attempt: 1,
                },
            ];
            store
                .append(WriteToken::recorder(), &workflow_id, &events, 0)
                .await?;
            let recorder = Recorder::resume_at(workflow_id.clone(), Arc::clone(&store), 3);
            let request = ActivityDispatch {
                namespace: String::from("default"),
                task_queue: String::from("default"),
                node: None,
                workflow_id: workflow_id.clone(),
                activity_id: ActivityId::from_sequence_position(0),
                name: "flaky".to_owned(),
                input: r#""in""#.to_owned(),
                config: config.to_owned(),
                attempt: super::FIRST_DELIVERY_ATTEMPT,
                labels: std::collections::BTreeMap::new(),
            };
            Ok(Self {
                store,
                seam: super::RetryRecorderSeam {
                    recorder: Arc::new(tokio::sync::Mutex::new(recorder)),
                    run_id,
                },
                request,
                workflow_id,
            })
        }

        async fn history(&self) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
            Ok(self.store.read_history(&self.workflow_id).await?)
        }
    }

    const FIXED_RETRY_CONFIG: &str =
        r#"{"retry":{"max_attempts":3,"backoff":{"kind":"fixed","delay_ms":2}}}"#;

    /// Retryable failure + budget left: the SAME ordinal re-dispatches at the
    /// incremented attempt after the non-terminal failure and the retry start
    /// are recorded — the observable per-attempt trail.
    #[tokio::test]
    async fn retryable_failure_redispatches_with_incremented_recorded_attempt() -> TestResult {
        let harness = RetryLoopHarness::seeded(FIXED_RETRY_CONFIG).await?;
        let dispatcher = ScriptedRetryDispatcher::new(vec![
            Err("retryable:stream reset".to_owned()),
            Ok(r#""done""#.to_owned()),
        ]);
        let outcome = super::dispatch_with_retries(
            &(Arc::clone(&dispatcher) as Arc<dyn ActivityDispatcher>),
            &harness.seam,
            &harness.request,
        )
        .await;

        assert!(
            matches!(
                &outcome.terminal,
                super::RetryLoopTerminal::Completed(payload) if payload == r#""done""#
            ),
            "the second attempt's success must be the delivered outcome"
        );
        assert_eq!(outcome.attempt, 2, "the completing attempt is attempt 2");
        assert_eq!(
            dispatcher.seen_attempts(),
            vec![1, 2],
            "the wire must carry the incremented attempt on the re-dispatch"
        );
        let history = harness.history().await?;
        assert!(
            matches!(
                history.get(3),
                Some(Event::ActivityFailed { error, attempt: 1, .. })
                    if error.kind == aion_core::ActivityErrorKind::Retryable
                        && error.message == "retryable:stream reset"
            ),
            "the failed attempt must be recorded as a NON-terminal retryable failure: {history:#?}"
        );
        assert!(
            matches!(
                history.get(4),
                Some(Event::ActivityStarted { attempt: 2, .. })
            ),
            "the retry delivery must record its ActivityStarted: {history:#?}"
        );
        Ok(())
    }

    /// Exhausted budget: the loop stops at `max_attempts`, the LAST reason is
    /// the delivered failure (verbatim), and the final attempt count rides
    /// with it.
    #[tokio::test]
    async fn exhausted_retry_budget_fails_with_last_reason_and_attempt_count() -> TestResult {
        let harness = RetryLoopHarness::seeded(FIXED_RETRY_CONFIG).await?;
        let dispatcher = ScriptedRetryDispatcher::new(vec![
            Err("retryable:reset one".to_owned()),
            Err("retryable:reset two".to_owned()),
            Err("retryable:reset three".to_owned()),
            Ok(r#""never delivered""#.to_owned()),
        ]);
        let outcome = super::dispatch_with_retries(
            &(Arc::clone(&dispatcher) as Arc<dyn ActivityDispatcher>),
            &harness.seam,
            &harness.request,
        )
        .await;

        assert!(
            matches!(
                &outcome.terminal,
                super::RetryLoopTerminal::Failed(reason) if reason == "retryable:reset three"
            ),
            "the LAST reason must surface verbatim"
        );
        assert_eq!(outcome.attempt, 3, "the budget is total attempts");
        assert_eq!(
            dispatcher.seen_attempts(),
            vec![1, 2, 3],
            "exactly max_attempts deliveries, one per attempt"
        );
        let history = harness.history().await?;
        // Two recorded retryable failures (attempts 1 and 2) and two retry
        // starts (attempts 2 and 3); the THIRD failure is the delivered
        // terminal, recorded by the awaiting workflow, not the loop.
        let retryable_failures = history
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    Event::ActivityFailed { error, .. }
                        if error.kind == aion_core::ActivityErrorKind::Retryable
                )
            })
            .count();
        assert_eq!(retryable_failures, 2, "{history:#?}");
        assert!(
            matches!(
                history.last(),
                Some(Event::ActivityStarted { attempt: 3, .. })
            ),
            "the final delivery's start must be recorded: {history:#?}"
        );
        Ok(())
    }

    /// Non-retryable failures behave exactly as before the retry loop:
    /// one delivery, no recorded retry trail, the reason delivered verbatim.
    #[tokio::test]
    async fn non_retryable_failure_fails_immediately_without_a_retry_trail() -> TestResult {
        let harness = RetryLoopHarness::seeded(FIXED_RETRY_CONFIG).await?;
        let dispatcher = ScriptedRetryDispatcher::new(vec![Err("terminal:bad request".to_owned())]);
        let outcome = super::dispatch_with_retries(
            &(Arc::clone(&dispatcher) as Arc<dyn ActivityDispatcher>),
            &harness.seam,
            &harness.request,
        )
        .await;

        assert!(matches!(
            &outcome.terminal,
            super::RetryLoopTerminal::Failed(reason) if reason == "terminal:bad request"
        ));
        assert_eq!(outcome.attempt, 1);
        assert_eq!(dispatcher.seen_attempts(), vec![1]);
        assert_eq!(
            harness.history().await?.len(),
            3,
            "no retry events may be recorded for a non-retryable failure"
        );
        Ok(())
    }

    /// No declared policy (`"retry": null`) keeps the SDK's run-exactly-once
    /// contract: a retryable-class failure is delivered after one attempt.
    #[tokio::test]
    async fn absent_policy_keeps_run_exactly_once_for_retryable_failures() -> TestResult {
        let harness = RetryLoopHarness::seeded(r#"{"retry":null}"#).await?;
        let dispatcher =
            ScriptedRetryDispatcher::new(vec![Err("retryable:stream reset".to_owned())]);
        let outcome = super::dispatch_with_retries(
            &(Arc::clone(&dispatcher) as Arc<dyn ActivityDispatcher>),
            &harness.seam,
            &harness.request,
        )
        .await;

        assert!(matches!(
            &outcome.terminal,
            super::RetryLoopTerminal::Failed(reason) if reason == "retryable:stream reset"
        ));
        assert_eq!(dispatcher.seen_attempts(), vec![1]);
        assert_eq!(harness.history().await?.len(), 3);
        Ok(())
    }

    /// #207 park-beats-retry: the parked sentinel stands the loop down BEFORE
    /// the retry-policy filter, even with budget left — no retry record, no
    /// delivered failure, no consumed budget. The durable log keeps only the
    /// seeded scheduled/started trail, byte-equivalent to a kill -9.
    #[tokio::test]
    async fn parked_dispatch_stands_down_without_recording_even_with_retry_budget() -> TestResult {
        let harness = RetryLoopHarness::seeded(FIXED_RETRY_CONFIG).await?;
        let dispatcher = ScriptedRetryDispatcher::new(vec![Err(
            crate::runtime::PARKED_ACTIVITY_REASON.to_owned(),
        )]);
        let outcome = super::dispatch_with_retries(
            &(Arc::clone(&dispatcher) as Arc<dyn ActivityDispatcher>),
            &harness.seam,
            &harness.request,
        )
        .await;

        assert!(
            matches!(outcome.terminal, super::RetryLoopTerminal::Parked),
            "a parked dispatch must stand down, never fail or retry"
        );
        assert_eq!(
            dispatcher.seen_attempts(),
            vec![1],
            "parking must not re-dispatch: no retry budget is consumed"
        );
        assert_eq!(
            harness.history().await?.len(),
            3,
            "nothing may be recorded for a parked dispatch — the durable log \
             must end at the dangling scheduled/started trail"
        );
        Ok(())
    }

    /// #207 sentinel is ephemeral end-to-end at the completion-task seam: a
    /// parked dispatch delivers NOTHING to the workflow process (no completion,
    /// no failure) and records nothing, so the process stays suspended for
    /// restart recovery.
    #[tokio::test(flavor = "multi_thread")]
    async fn parked_dispatch_delivers_nothing_to_the_workflow_process() -> TestResult {
        let harness = RetryLoopHarness::seeded(r#"{"retry":null}"#).await?;
        let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
        let pid = runtime.spawn_test_process()?;
        let dispatcher = ScriptedRetryDispatcher::new(vec![Err(
            crate::runtime::PARKED_ACTIVITY_REASON.to_owned(),
        )]);
        spawn_completion_task(
            &tokio::runtime::Handle::current(),
            Arc::clone(&runtime),
            Arc::clone(&dispatcher) as Arc<dyn ActivityDispatcher>,
            super::RetryRecorderSeam {
                recorder: Arc::clone(&harness.seam.recorder),
                run_id: harness.seam.run_id.clone(),
            },
            pid,
            super::correlation_id(0),
            harness.request.clone(),
        );
        // Give the completion task ample time to run to its terminal; a parked
        // dispatch must leave the runtime maps empty (nothing delivered).
        for _ in 0_u32..40 {
            if dispatcher.seen_attempts().len() == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            runtime.take_activity_result(pid, 0)?.is_none(),
            "a parked dispatch must never deliver a completion"
        );
        assert!(
            runtime.take_activity_error(pid, 0)?.is_none(),
            "the parked sentinel must never be delivered to workflow code"
        );
        assert_eq!(
            harness.history().await?.len(),
            3,
            "the durable log must be untouched by a parked dispatch"
        );
        runtime.shutdown()?;
        Ok(())
    }

    /// Settle race: a terminal recorded by another path (a `with_timeout`
    /// expiry) while the loop runs must stop the loop — no retry record may
    /// ever land after the ordinal's terminal.
    #[tokio::test]
    async fn retry_loop_aborts_without_recording_once_the_activity_settled() -> TestResult {
        let harness = RetryLoopHarness::seeded(FIXED_RETRY_CONFIG).await?;
        // The workflow thread already recorded the ordinal's durable timeout
        // terminal (seq 4) before the loop's first failure comes back.
        let timeout_terminal = vec![Event::ActivityFailed {
            envelope: envelope(&harness.workflow_id, 4),
            activity_id: ActivityId::from_sequence_position(0),
            error: aion_core::ActivityError {
                kind: aion_core::ActivityErrorKind::Terminal,
                message: "timeout:deadline expired".to_owned(),
                details: None,
            },
            attempt: 1,
        }];
        harness
            .store
            .append(
                WriteToken::recorder(),
                &harness.workflow_id,
                &timeout_terminal,
                3,
            )
            .await?;
        let dispatcher =
            ScriptedRetryDispatcher::new(vec![Err("retryable:stream reset".to_owned())]);
        let outcome = super::dispatch_with_retries(
            &(Arc::clone(&dispatcher) as Arc<dyn ActivityDispatcher>),
            &harness.seam,
            &harness.request,
        )
        .await;

        assert!(
            matches!(outcome.terminal, super::RetryLoopTerminal::SettledElsewhere),
            "the loop must observe the recorded terminal and stand down"
        );
        assert_eq!(
            harness.history().await?.len(),
            4,
            "nothing may be recorded after the ordinal's terminal"
        );
        Ok(())
    }

    /// The completion task notes the final attempt where the awaiting NIF
    /// takes it, and the recorded terminal carries it (NOI-0 fidelity across
    /// the retry loop).
    #[tokio::test(flavor = "multi_thread")]
    async fn awaited_terminal_records_the_noted_final_attempt() -> TestResult {
        let backing = Arc::new(StaleReadStore::new(0));
        let store: Arc<dyn EventStore> = Arc::clone(&backing) as Arc<dyn EventStore>;
        let (workflow_id, run_id) = seed_pending_activity_then_deadline(&store, 7).await?;
        let harness =
            AwaitHarness::over_store(Arc::clone(&store), workflow_id.clone(), run_id).await?;
        harness
            .runtime
            .deliver_activity_failure_message_with_attempt(
                harness.pid,
                "activity:0",
                "retryable:reset three".to_owned(),
                Some(3),
            )?;

        assert_eq!(
            harness.step(),
            Ok(ActivityAwaitStep::Failed(
                "retryable:reset three".to_owned()
            ))
        );
        let history = store.read_history(&workflow_id).await?;
        assert!(
            matches!(
                history.last(),
                Some(Event::ActivityFailed { attempt: 3, error, .. })
                    if error.message == "retryable:reset three"
            ),
            "the recorded terminal must carry the noted final attempt: {history:#?}"
        );
        harness.shutdown()
    }
}
