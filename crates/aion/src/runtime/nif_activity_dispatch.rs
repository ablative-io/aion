//! Two-phase activity dispatch NIFs.

use std::sync::Arc;

use crate::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use crate::durability::{Command, CorrelationKey, Resolution, ResolveOutcome};
use crate::runtime::nif_activity::{
    activity_error, activity_id_from_correlation, context_error_term, correlation_id,
    decode_string_arg, error_result_term, json_payload, labels_from_config, ok_result_term,
    record_started, runtime_context,
};
use crate::runtime::nif_context::NifContext;
use aion_core::{ActivityId, Payload};
use beamr::native::ProcessContext;
use beamr::term::Term;
use futures::FutureExt;

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

/// First delivery: every dispatch issued from workflow code is attempt 1.
///
/// The retry executor (unbuilt; the retry POLICY rides in the `config` JSON,
/// `gleam/aion_flow/src/aion/workflow/run.gleam`, and is consumed by nothing
/// yet) re-invokes with the incremented attempt when it lands — the wire and
/// the [`ActivityDispatcher`] seam are ready for it. This is the single
/// documented producer-side constant; no consumer guesses an attempt.
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
            record_started(
                ctx,
                &context,
                activity_id.clone(),
                call.name.clone(),
                input_payload,
            )?;
            let labels = labels_from_config(&call.config);
            let request = ActivityDispatch {
                namespace,
                workflow_id: context.workflow_id().clone(),
                activity_id,
                name: call.name,
                input: call.input,
                config: call.config,
                attempt: call.attempt,
                labels,
            };
            spawn_completion_task(
                tokio_handle,
                runtime,
                dispatcher,
                context.pid(),
                correlation.clone(),
                request,
            );
            Ok(ok_result_term(ctx, correlation.as_bytes()).unwrap_or(Term::NIL))
        }
    }
}

pub(super) fn spawn_completion_task(
    tokio_handle: &tokio::runtime::Handle,
    runtime: Arc<crate::RuntimeHandle>,
    dispatcher: Arc<dyn ActivityDispatcher>,
    workflow_pid: u64,
    correlation_id: String,
    request: ActivityDispatch,
) {
    let future = dispatcher
        .dispatch_async(request)
        .map(move |result| {
            match result {
            Ok(payload) => {
                if let Err(error) = runtime.deliver_activity_completion_message(
                    workflow_pid,
                    &correlation_id,
                    payload,
                ) {
                    tracing::warn!(%error, workflow_pid, correlation_id, "activity completion delivery failed");
                }
            }
            Err(reason) => {
                if let Err(error) = runtime.deliver_activity_failure_message(
                    workflow_pid,
                    &correlation_id,
                    reason,
                ) {
                    tracing::warn!(%error, workflow_pid, correlation_id, "activity failure delivery failed");
                }
            }
        }
    });
    tokio_handle.spawn(future);
}

fn await_activity_result_with_context(
    state: &crate::runtime::EngineNifState,
    mut context: NifContext,
    runtime: &Arc<crate::RuntimeHandle>,
    process_context: &mut ProcessContext,
    correlation: &str,
) -> Result<Term, Term> {
    // A query handler must not nest into another await; refuse before any
    // marker is consumed or resolution attempted.
    if let Err(error) = crate::runtime::nif_query_pump::ensure_not_servicing_query(
        state,
        context.pid(),
        "await_activity_result",
    ) {
        return Ok(error_result_term(process_context, &error).unwrap_or(Term::NIL));
    }
    // Queries first (Q6), and deliberately BEFORE the recorded-resolution
    // fast path below: a tight replay loop whose awaits all resolve from
    // history instantly must still drain queued queries at each yield point.
    if let Some(sentinel) =
        crate::runtime::nif_query_pump::take_pending_query_sentinel(state, context.pid())
    {
        return Ok(error_result_term(process_context, &sentinel).unwrap_or(Term::NIL));
    }
    let activity_id = activity_id_from_correlation(process_context, correlation)?;
    let step = await_activity_step(state, &mut context, runtime, &activity_id, || {
        super::nif_wake::consume_wake_marker(process_context, runtime);
    });
    match step {
        Ok(ActivityAwaitStep::Completed(bytes)) => {
            Ok(ok_result_term(process_context, &bytes).unwrap_or(Term::NIL))
        }
        Ok(ActivityAwaitStep::Failed(message)) => {
            Ok(error_result_term(process_context, &message).unwrap_or(Term::NIL))
        }
        Ok(ActivityAwaitStep::Suspend) => {
            process_context.request_suspend(None);
            Ok(Term::NIL)
        }
        Err(message) => Err(error_result_term(process_context, &message).unwrap_or(Term::NIL)),
    }
}

/// Outcome of one ProcessContext-free `await_activity_result` resolution
/// step, invoked fresh on every mailbox wake by the NIF shell above.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ActivityAwaitStep {
    /// The recorded (or just-recorded) completion payload bytes.
    Completed(Vec<u8>),
    /// A workflow-visible `{error, _}`: the recorded (or just-recorded)
    /// terminal failure message, or a recorded non-activity resolution.
    Failed(String),
    /// Park the calling process; a mailbox wake re-invokes the native.
    Suspend,
}

/// One activity-await resolution pass.
///
/// Recorded terminal first — it IS the live run's decision (this await
/// records completions, failures, and the durable timeout failure itself,
/// synchronously, before workflow code observes the branch), so replay reads
/// the decision and no deadline-vs-terminal seq ordering is needed on the
/// recorded path (unlike `await_child`/`receive_signal`, whose arrivals are
/// recorded by racing third parties). Then a takeable runtime-map completion
/// (recorded now), then the enclosing-scope expiry, then suspend.
///
/// `consume_wake_marker` runs once per live pass, only when no recorded
/// terminal resolves the await: markers are pure wakes and the completion
/// state lives in the runtime's keyed maps, so any marker (even one destined
/// for another await) is safe to take — leaving it queued would insta-rewake
/// the suspend into a busy spin.
pub(super) fn await_activity_step(
    state: &crate::runtime::EngineNifState,
    context: &mut NifContext,
    runtime: &crate::RuntimeHandle,
    activity_id: &ActivityId,
    consume_wake_marker: impl FnOnce(),
) -> Result<ActivityAwaitStep, String> {
    if let Some(recorded) = recorded_resolution_for(context, activity_id)? {
        return Ok(recorded_step(recorded));
    }
    consume_wake_marker();
    if let Some(step) = take_runtime_completion(context, runtime, activity_id.clone())? {
        return Ok(step);
    }
    // An expired enclosing with_timeout deadline aborts the await instead of
    // re-suspending; the failure is recorded durably so replay returns it
    // verbatim. The expiry decision is a pure function of the RESOLUTION
    // snapshot (`context.history()`), never a fresh store read: this
    // resolution observed neither the activity terminal nor the deadline's
    // `TimerFired`, and deciding the branch from a newer snapshot than the
    // one the resolution settled on is the N-1 defect family. A deadline
    // firing after the snapshot is settled by the wake it triggers, whose
    // fresh snapshot re-enters this step.
    if crate::runtime::nif_timeout::expired_scope_deadline(state, context.pid(), context.history())
        .is_some()
    {
        let message = crate::runtime::nif_timeout::SCOPE_EXPIRED_MESSAGE.to_owned();
        context
            .record_activity_failed(
                chrono::Utc::now(),
                activity_id.clone(),
                activity_error(message.clone()),
                1,
            )
            .map_err(|error| error.error_reason())?;
        return Ok(ActivityAwaitStep::Failed(message));
    }
    Ok(ActivityAwaitStep::Suspend)
}

fn recorded_resolution_for(
    context: &mut NifContext,
    activity_id: &ActivityId,
) -> Result<Option<Resolution>, String> {
    let ordinal = activity_id.sequence_position();
    let input = Payload::from_json(&serde_json::Value::Null)
        .map_err(|error| format!("await_activity_result replay input: {error}"))?;
    match context
        .resolve_command(Command::RunActivity {
            key: CorrelationKey::Activity(ordinal),
            activity_type: "await_activity_result".to_owned(),
            input,
        })
        .map_err(|error| error.error_reason())?
    {
        ResolveOutcome::Recorded(resolution) => Ok(Some(resolution)),
        ResolveOutcome::ResumeLive => Ok(None),
    }
}

fn recorded_step(resolution: Resolution) -> ActivityAwaitStep {
    match resolution {
        Resolution::ActivityCompleted(payload) => {
            ActivityAwaitStep::Completed(payload.bytes().to_vec())
        }
        Resolution::ActivityFailedTerminal(error) => ActivityAwaitStep::Failed(error.message),
        other => ActivityAwaitStep::Failed(format!(
            "await_activity_result: recorded non-activity resolution {other:?}"
        )),
    }
}

fn take_runtime_completion(
    context: &NifContext,
    runtime: &crate::RuntimeHandle,
    activity_id: ActivityId,
) -> Result<Option<ActivityAwaitStep>, String> {
    if let Some(payload) =
        runtime.take_activity_result(context.pid(), activity_id.sequence_position())
    {
        context
            .record_activity_completed(chrono::Utc::now(), activity_id, payload.clone())
            .map_err(|error| error.error_reason())?;
        return Ok(Some(ActivityAwaitStep::Completed(payload.bytes().to_vec())));
    }
    if let Some(error) = runtime.take_activity_error(context.pid(), activity_id.sequence_position())
    {
        context
            .record_activity_failed(
                chrono::Utc::now(),
                activity_id,
                activity_error(error.message.clone()),
                1,
            )
            .map_err(|record_error| record_error.error_reason())?;
        return Ok(Some(ActivityAwaitStep::Failed(error.message)));
    }
    Ok(None)
}

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
                .map_err(|error| error.error_reason())?;
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
            },
            Event::ActivityStarted {
                envelope: envelope(&workflow_id, 3),
                activity_id: ActivityId::from_sequence_position(0),
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
            spawn_completion_task(
                &tokio::runtime::Handle::current(),
                Arc::clone(&runtime),
                dispatcher,
                pid,
                super::correlation_id(0),
                ActivityDispatch {
                    namespace: String::from("default"),
                    workflow_id: WorkflowId::new_v4(),
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
                if let Some(delivered) = runtime.take_activity_result(pid, 0) {
                    payload = Some(delivered.bytes().to_vec());
                    break;
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
}
