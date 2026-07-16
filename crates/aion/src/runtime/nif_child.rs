//! Child-workflow NIF bridge implementations.
//!
//! `spawn_child` is record-then-spawn (#56): the live path pre-allocates the
//! child workflow id, records `ChildWorkflowStarted` through the parent's
//! single Recorder, and only then starts the child under that id, so replay
//! resolves the spawn from history and a crash in the window is repaired by
//! the startup recovery sweep.
//!
//! `await_child` is a two-phase suspending native: it never blocks a
//! scheduler thread. The first live arrival validates the child, arms the
//! engine-side child-terminal watcher, pins the await identity, and parks
//! the process via `request_suspend`. Every mailbox wake re-invokes the
//! native from the top; resolution always reads the parent-side terminal
//! the watcher recorded durably before delivering the wake marker.

use std::sync::Arc;

use aion_core::{ContentType, Payload, WorkflowError, WorkflowId};
use beamr::atom::Atom;
use beamr::native::ProcessContext;
use beamr::term::Term;
use beamr::term::binary_ref::BinaryRef;
use chrono::Utc;

use crate::durability::{Command, CorrelationKey, Resolution, ResolveOutcome};
use crate::engine_seam::{ChildWorkflowSpawnRequest, EngineHandle};
use crate::runtime::nif_child_engine::{ChildNifBridge, NifChildEngine};
use crate::runtime::nif_child_watch::{ChildWatchContext, arm_child_terminal_watch};
use crate::runtime::nif_state::{EngineNifState, PendingAwait};

use super::nif_context::{NifContext, NifContextError};

/// Installs the engine-owned dependencies used by child workflow NIFs.
pub(crate) fn install_child_nif_bridge(
    state: &super::nif_state::EngineNifState,
    bridge: Arc<ChildNifBridge>,
) {
    match state.child_bridge.write() {
        Ok(mut slot) => *slot = Some(bridge),
        Err(poisoned) => *poisoned.into_inner() = Some(bridge),
    }
}

/// NIF backing `aion_flow_ffi:spawn_child/3`.
pub(super) fn spawn_child_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    let result = run_spawn_child(args, ctx);
    Ok(checked_child_result(ctx, result, "spawn_child"))
}

/// NIF backing `aion_flow_ffi:await_child/1`.
///
/// Runs on the normal schedulers: it parks via `request_suspend` instead of
/// blocking, so a dirty thread is never held for the child's lifetime.
pub(super) fn await_child_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    match run_await_child(args, ctx) {
        Ok(AwaitChildOutcome::Resolved(term)) => Ok(term),
        Ok(AwaitChildOutcome::Suspend) => {
            // Park the process; the next mailbox wake re-invokes this native
            // from the top with the await identity pinned. The NIL return is
            // never observed by workflow code.
            ctx.request_suspend(None);
            Ok(Term::NIL)
        }
        Err(message) => {
            Ok(error_result_term(ctx, &format!("await_child:{message}")).unwrap_or(Term::NIL))
        }
    }
}

fn run_spawn_child(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, String> {
    require_arity("spawn_child", args, 3)?;
    let workflow_type =
        decode_string_arg(args[0]).map_err(|error| format!("workflow_type:{error}"))?;
    let input = decode_payload_arg(args[1]).map_err(|error| format!("input:{error}"))?;
    decode_string_arg(args[2]).map_err(|error| format!("options:{error}"))?;
    let bridge = child_bridge(ctx)?;
    let pid = ctx.pid().ok_or_else(|| "missing_caller_pid".to_owned())?;
    // spawn_child records `ChildWorkflowStarted`; a query handler must stay
    // read-only.
    let state = super::nif_state::engine_nif_state(ctx)?;
    super::nif_query_pump::ensure_not_servicing_query(&state, pid, "spawn_child")?;
    let mut nif = new_context(&bridge, pid)?;
    let key = next_child_key(&nif);
    let command = Command::SpawnChild {
        key,
        workflow_type: workflow_type.clone(),
        input: input.clone(),
    };

    match nif
        .resolve_command(command)
        .map_err(|error| context_error(&error))?
    {
        ResolveOutcome::Recorded(Resolution::ChildStarted(child_id)) => {
            term_or_encoding_error(ok_result_term(ctx, &child_id.to_string()))
        }
        ResolveOutcome::Recorded(other) => {
            Err(format!("unexpected_child_spawn_resolution:{other:?}"))
        }
        ResolveOutcome::ResumeLive => {
            // Resolve within the parent's exact package version. The durable
            // record carries that pin through retry, recovery, and adoption.
            let package_version = bridge
                .package_version_for_child(&workflow_type, nif.workflow_handle().loaded_version())
                .map_err(|error| format!("child_version_resolution:{error}"))?
                .ok_or_else(|| format!("child_workflow_type_not_loaded:{workflow_type}"))?;
            // Record-then-spawn (#56): the id is recorded nondeterminism —
            // drawn once here, durably recorded before any observable use,
            // returned from history on every replay.
            let child_workflow_id = WorkflowId::new_v4();
            nif.block_on_recorder(|recorder| {
                let child_workflow_id = child_workflow_id.clone();
                let workflow_type = workflow_type.clone();
                let input = input.clone();
                let package_version = package_version.clone();
                Box::pin(async move {
                    recorder
                        .record_child_workflow_started(
                            Utc::now(),
                            child_workflow_id,
                            workflow_type,
                            input,
                            package_version,
                        )
                        .await
                })
            })
            .map_err(|error| context_error(&error))?;

            // From the durable record on, the start is an engine-internal
            // obligation, never a workflow-visible outcome (F3): replay
            // resolves this spawn from the recorded event as success, so the
            // live path must report success too — a start failure here is
            // recovered in the background (or by the next epoch's startup
            // sweep), exactly like the crash-in-the-window case.
            let engine = NifChildEngine::new(Arc::clone(&bridge), nif.workflow_handle().clone());
            let request = ChildWorkflowSpawnRequest {
                parent_workflow_id: nif.workflow_id().clone(),
                child_workflow_id: child_workflow_id.clone(),
                workflow_type: workflow_type.clone(),
                input: input.clone(),
                package_version: package_version.clone(),
            };
            match engine.spawn_child_workflow(request) {
                Ok(result) if result.child_workflow_id == child_workflow_id => {}
                Ok(result) => {
                    // Engine invariant violation (F6): the start path must
                    // echo the pre-allocated identity it was given. Recover
                    // through the same internal path as a failed start; the
                    // recovery task verifies store truth before retrying.
                    tracing::error!(
                        parent_workflow_id = %nif.workflow_id(),
                        recorded_child_workflow_id = %child_workflow_id,
                        started_workflow_id = %result.child_workflow_id,
                        "engine invariant violation: child start echoed a different workflow id"
                    );
                    recover_spawn_in_background(
                        &bridge,
                        &nif,
                        &child_workflow_id,
                        workflow_type,
                        input,
                        package_version,
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        parent_workflow_id = %nif.workflow_id(),
                        child_workflow_id = %child_workflow_id,
                        error = %error,
                        "recorded child start failed; recovering in the background"
                    );
                    recover_spawn_in_background(
                        &bridge,
                        &nif,
                        &child_workflow_id,
                        workflow_type,
                        input,
                        package_version,
                    );
                }
            }
            term_or_encoding_error(ok_result_term(ctx, &child_workflow_id.to_string()))
        }
    }
}

/// Hand a failed post-record child start to the background recovery task.
fn recover_spawn_in_background(
    bridge: &Arc<ChildNifBridge>,
    nif: &NifContext,
    child_workflow_id: &WorkflowId,
    workflow_type: String,
    input: Payload,
    package_version: aion_core::PackageVersion,
) {
    let armed = super::nif_child_spawn_retry::ensure_child_started_in_background(
        bridge,
        super::nif_child_spawn_retry::RecordedChildSpawn {
            parent_workflow_id: nif.workflow_id().clone(),
            child_workflow_id: child_workflow_id.clone(),
            workflow_type,
            input,
            package_version,
            namespace: nif.workflow_handle().namespace().to_owned(),
        },
    );
    if !armed {
        // Shutdown gate or an already-armed task: either the next epoch's
        // startup sweep repairs the recorded-but-never-started child, or the
        // existing task already owns the obligation.
        tracing::debug!(
            child_workflow_id = %child_workflow_id,
            "spawn recovery not armed (already armed or epoch closing)"
        );
    }
}

/// Outcome of one `await_child` invocation.
enum AwaitChildOutcome {
    /// A complete `{ok, _}`/`{error, _}` result term for workflow code.
    Resolved(Term),
    /// Park the calling process; a mailbox wake re-invokes the native.
    Suspend,
}

/// One ProcessContext-free `await_child` resolution step.
#[derive(Debug, PartialEq, Eq)]
enum AwaitChildStep {
    /// A pending query's sentinel payload for `{error, <<"aion_query:...">>}`.
    QuerySentinel(String),
    /// The D4 result envelope payload (`ok:`/`error:` prefixed) for `{ok, _}`.
    ChildResolved(String),
    /// The enclosing `with_timeout` scope expired: `{error, message}`.
    ScopeExpired(String),
    /// Park the calling process; the watcher is armed and the pin is set.
    Suspend,
}

fn run_await_child(args: &[Term], ctx: &mut ProcessContext) -> Result<AwaitChildOutcome, String> {
    require_arity("await_child", args, 1)?;
    let child_workflow_id = parse_workflow_id(&decode_string_arg(args[0])?)?;
    let bridge = child_bridge(ctx)?;
    let pid = ctx.pid().ok_or_else(|| "missing_caller_pid".to_owned())?;
    let state = super::nif_state::engine_nif_state(ctx)?;
    // A query handler must not nest into another await; refuse before any
    // marker is consumed.
    super::nif_query_pump::ensure_not_servicing_query(&state, pid, "await_child")?;
    // One wake marker is consumed per invocation; leaving it queued would
    // insta-rewake the suspend below into a busy spin.
    super::nif_wake::consume_wake_marker(ctx, &bridge.runtime());
    match await_child_step(&state, &bridge, pid, &child_workflow_id)? {
        AwaitChildStep::QuerySentinel(sentinel) => Ok(AwaitChildOutcome::Resolved(
            error_result_term(ctx, &sentinel).unwrap_or(Term::NIL),
        )),
        AwaitChildStep::ChildResolved(envelope) => Ok(AwaitChildOutcome::Resolved(
            term_or_encoding_error(ok_result_term(ctx, &envelope))?,
        )),
        AwaitChildStep::ScopeExpired(message) => Ok(AwaitChildOutcome::Resolved(
            error_result_term(ctx, &message).unwrap_or(Term::NIL),
        )),
        AwaitChildStep::Suspend => Ok(AwaitChildOutcome::Suspend),
    }
}

/// Two-phase `await_child` resolution, invoked fresh on every wake.
///
/// Order is load-bearing: queries first (before any recorded-result fast
/// path, Q6), then the pin check, then replay resolution from the parent's
/// recorded history, then the expired-scope abort, and only then the
/// idempotent watcher arm + pin + suspend.
fn await_child_step(
    state: &EngineNifState,
    bridge: &Arc<ChildNifBridge>,
    pid: u64,
    child_workflow_id: &WorkflowId,
) -> Result<AwaitChildStep, String> {
    // Queries first (Q6): a pending query is serviced before this await's
    // own resolution, so operator queries are answered while the parent is
    // parked on a child. The pin is deliberately untouched.
    if let Some(sentinel) = super::nif_query_pump::take_pending_query_sentinel(state, pid) {
        return Ok(AwaitChildStep::QuerySentinel(sentinel));
    }

    // Pin check: re-entries must resolve the same logical await.
    match state.pending_awaits.get(&pid).map(|entry| entry.clone()) {
        Some(PendingAwait::Child {
            child_workflow_id: pinned,
        }) => {
            if pinned != *child_workflow_id {
                return Err(format!(
                    "process is pinned to a pending await for child {pinned}, \
                     not {child_workflow_id}"
                ));
            }
        }
        Some(PendingAwait::Sleep { .. }) => {
            return Err("process is pinned to a pending sleep await".to_owned());
        }
        Some(PendingAwait::Signal { .. }) => {
            return Err("process is pinned to a pending signal await".to_owned());
        }
        Some(PendingAwait::Collect { .. }) => {
            return Err("process is pinned to a pending collect await".to_owned());
        }
        None => {}
    }

    let mut nif = new_context(bridge, pid)?;
    let command = Command::AwaitChild {
        child_workflow_id: child_workflow_id.clone(),
    };
    match nif
        .resolve_command(command)
        .map_err(|error| context_error(&error))?
    {
        // D4 envelope: child success and child failure are both `{ok, _}`
        // data with `ok:`/`error:` payload prefixes (the SDK decode
        // contract); `{error, _}` is reserved for engine faults.
        ResolveOutcome::Recorded(Resolution::ChildCompleted(result)) => {
            if let Some(message) =
                scope_expired_before_child_terminal(state, bridge, &nif, pid, child_workflow_id)
            {
                return Ok(AwaitChildStep::ScopeExpired(message));
            }
            state.pending_awaits.remove(&pid);
            let payload = payload_text(&result)?;
            Ok(AwaitChildStep::ChildResolved(format!("ok:{payload}")))
        }
        ResolveOutcome::Recorded(Resolution::ChildFailed(error)) => {
            if let Some(message) =
                scope_expired_before_child_terminal(state, bridge, &nif, pid, child_workflow_id)
            {
                return Ok(AwaitChildStep::ScopeExpired(message));
            }
            state.pending_awaits.remove(&pid);
            let details = workflow_error_text(&error);
            Ok(AwaitChildStep::ChildResolved(format!("error:{details}")))
        }
        ResolveOutcome::Recorded(other) => {
            Err(format!("unexpected_child_await_resolution:{other:?}"))
        }
        ResolveOutcome::ResumeLive => {
            // An expired enclosing with_timeout deadline aborts the await:
            // the typed scope error is returned, the pin is released, the
            // armed watcher is disarmed (F1a — a later child terminal must
            // not be recorded into history the live run already branched
            // away from), and the child is left running (D-1).
            //
            // The expiry decision is a pure function of the RESOLUTION
            // snapshot (`nif.history()`), never a fresh store read: this
            // resolution observed neither a child terminal nor the deadline
            // `TimerFired`, and deciding the branch from a newer snapshot
            // diverges from replay. The race that breaks the fresh read —
            // watcher records the child terminal (seq c), the timer service
            // records the deadline `TimerFired` (seq d), c < d, both after
            // this snapshot — made live take the timeout branch while replay
            // (Recorded + F1b, d < c false) took the child branch (N-1). A
            // snapshot lacking both events suspends instead and converges to
            // the Recorded path on the next wake.
            if super::nif_timeout::expired_scope_deadline(state, pid, nif.history()).is_some() {
                state.pending_awaits.remove(&pid);
                bridge.child_tasks().abort_watch(pid, child_workflow_id);
                return Ok(AwaitChildStep::ScopeExpired(
                    super::nif_timeout::SCOPE_EXPIRED_MESSAGE.to_owned(),
                ));
            }
            ensure_awaitable_child(bridge, &nif, child_workflow_id)?;
            let context = ChildWatchContext {
                store: bridge.store(),
                registry: bridge.registry_arc(),
                runtime: bridge.runtime(),
                tasks: bridge.child_tasks(),
                backoff: bridge.watch_backoff(),
                parent: nif.workflow_handle(),
                child_workflow_id: child_workflow_id.clone(),
            };
            // Idempotent: re-entries while the watcher runs are no-ops.
            arm_child_terminal_watch(context);
            state.pending_awaits.insert(
                pid,
                PendingAwait::Child {
                    child_workflow_id: child_workflow_id.clone(),
                },
            );
            Ok(AwaitChildStep::Suspend)
        }
    }
}

/// F1b — order an enclosing expired `with_timeout` deadline against the
/// recorded child terminal, identically on the live and replayed paths.
///
/// The watcher records the child terminal asynchronously, so a deadline
/// `TimerFired` and a `ChildWorkflowCompleted/Failed` can both be in the
/// run segment when this await resolves. History order is the truth both
/// paths share: if the deadline fired before the child terminal was
/// recorded, the await takes the timeout branch (releasing the pin and
/// disarming the watcher); only a terminal recorded *before* the deadline
/// resolves as the child's outcome. Without this rule a live run that
/// timed out (terminal recorded later by a racing watcher) would replay
/// into the success branch — opposite branches live vs replay.
fn scope_expired_before_child_terminal(
    state: &EngineNifState,
    bridge: &Arc<ChildNifBridge>,
    nif: &NifContext,
    pid: u64,
    child_workflow_id: &WorkflowId,
) -> Option<String> {
    let deadline = super::nif_timeout::expired_scope_deadline(state, pid, nif.history())?;
    let child_terminal_seq = nif.history().iter().find_map(|event| match event {
        aion_core::Event::ChildWorkflowCompleted {
            envelope,
            child_workflow_id: recorded,
            ..
        }
        | aion_core::Event::ChildWorkflowFailed {
            envelope,
            child_workflow_id: recorded,
            ..
        } if recorded == child_workflow_id => Some(envelope.seq),
        _ => None,
    });
    let deadline_first = match (deadline, child_terminal_seq) {
        (super::nif_timeout::ExpiredScopeDeadline::RecordedAt(fired_seq), Some(child_seq)) => {
            fired_seq < child_seq
        }
        // An expiry without a recorded position orders before every arrival
        // (replay-derived scope state), and a resolved child terminal must be
        // in the segment — if it cannot be located the deterministic choice
        // is the deadline branch either way.
        (super::nif_timeout::ExpiredScopeDeadline::Unordered, _)
        | (super::nif_timeout::ExpiredScopeDeadline::RecordedAt(_), None) => true,
    };
    if !deadline_first {
        return None;
    }
    state.pending_awaits.remove(&pid);
    bridge.child_tasks().abort_watch(pid, child_workflow_id);
    Some(super::nif_timeout::SCOPE_EXPIRED_MESSAGE.to_owned())
}

/// Reject awaiting a child the engine has no trace of, before suspending.
///
/// A child is awaitable when the parent's current run segment records its
/// `ChildWorkflowStarted` (this also covers the recovery-sweep window where
/// the child's own history does not exist yet), when the registry holds a
/// handle for it, or when its own durable history exists (cross-run awaits
/// after the handle is gone). A workflow id matching none of these would
/// park the caller forever.
fn ensure_awaitable_child(
    bridge: &ChildNifBridge,
    nif: &NifContext,
    child_workflow_id: &WorkflowId,
) -> Result<(), String> {
    let started_in_segment = nif.history().iter().any(|event| {
        matches!(
            event,
            aion_core::Event::ChildWorkflowStarted {
                child_workflow_id: recorded,
                ..
            } if recorded == child_workflow_id
        )
    });
    if started_in_segment {
        return Ok(());
    }
    let registered = bridge
        .registry()
        .list()
        .map_err(|error| format!("registry:{error}"))?
        .into_iter()
        .any(|handle| handle.workflow_id() == child_workflow_id);
    if registered {
        return Ok(());
    }
    let history_exists = !bridge
        .tokio_handle()
        .block_on(bridge.store().read_history(child_workflow_id))
        .map_err(|error| format!("store:{error}"))?
        .is_empty();
    if history_exists {
        return Ok(());
    }
    Err(format!("unknown_child_workflow:{child_workflow_id}"))
}

fn checked_child_result(
    ctx: &mut ProcessContext,
    result: Result<Term, String>,
    name: &str,
) -> Term {
    match result {
        Ok(term) => term,
        Err(message) => error_result_term(ctx, &format!("{name}:{message}")).unwrap_or(Term::NIL),
    }
}

fn child_bridge(ctx: &ProcessContext) -> Result<Arc<ChildNifBridge>, String> {
    let state = super::nif_state::engine_nif_state(ctx)?;
    child_bridge_from_state(&state)
}

fn child_bridge_from_state(state: &EngineNifState) -> Result<Arc<ChildNifBridge>, String> {
    let slot = match state.child_bridge.read() {
        Ok(slot) => slot.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    slot.ok_or_else(|| "no_child_nif_bridge_configured".to_owned())
}

fn new_context(bridge: &ChildNifBridge, pid: u64) -> Result<NifContext, String> {
    NifContext::new_with_history_store(
        pid,
        bridge.registry(),
        bridge.tokio_handle(),
        Some(bridge.store()),
        bridge.watch_backoff(),
    )
    .map_err(|error| context_error(&error))
}

/// Derives the spawn's correlation key from the run-scoped child ordinal.
///
/// The ordinal counter lives on the workflow handle and restarts at zero for
/// every fresh run (live start, crash-recovery re-spawn, continue-as-new
/// replacement), so the n-th `spawn_child` call always correlates with the
/// n-th recorded `ChildWorkflowStarted` in the run segment — independent of
/// the recorder's sequence head, which moves with asynchronous-arrival
/// appends and resumes at the full history head after recovery.
fn next_child_key(nif: &NifContext) -> CorrelationKey {
    CorrelationKey::Child(nif.next_child_ordinal())
}

fn context_error(error: &NifContextError) -> String {
    error.error_reason()
}

fn parse_workflow_id(value: &str) -> Result<WorkflowId, String> {
    uuid::Uuid::parse_str(value)
        .map(WorkflowId::new)
        .map_err(|error| format!("invalid_child_workflow_id:{error}"))
}

fn decode_payload_arg(term: Term) -> Result<Payload, String> {
    decode_string_arg(term).map(|value| Payload::new(ContentType::Json, value.into_bytes()))
}

fn term_or_encoding_error(term: Option<Term>) -> Result<Term, String> {
    term.ok_or_else(|| "failed_to_encode_child_nif_result".to_owned())
}

fn decode_string_arg(term: Term) -> Result<String, String> {
    let bin = BinaryRef::new(term).ok_or_else(|| "argument is not a binary".to_owned())?;
    String::from_utf8(bin.as_bytes().to_vec()).map_err(|_| "argument is not valid UTF-8".to_owned())
}

fn require_arity(name: &str, args: &[Term], expected: usize) -> Result<(), String> {
    if args.len() == expected {
        Ok(())
    } else {
        Err(format!(
            "{name}: expected {expected} arguments, got {}",
            args.len()
        ))
    }
}

fn payload_text(payload: &Payload) -> Result<String, String> {
    String::from_utf8(payload.bytes().to_vec()).map_err(|_| "payload is not valid UTF-8".to_owned())
}

fn workflow_error_text(error: &WorkflowError) -> String {
    match &error.details {
        Some(details) => payload_text(details).unwrap_or_else(|_| error.message.clone()),
        None => error.message.clone(),
    }
}

/// Build `{ok, <<value>>}` on the calling process heap.
///
/// Result terms are allocated through the [`ProcessContext`] allocators:
/// attached (normal-scheduler) calls get GC-traced process-heap terms, and
/// detached (dirty) calls get owned blocks the dirty-result bridge copies
/// onto the process heap. Nothing is parked in thread-locals — beamr's
/// moving GC never traces out-of-heap pointers, so a parked heap either
/// leaks for the scheduler thread's lifetime or dangles once cleared while
/// workflow code still references the term (N-6).
///
/// Allocation may collect on attached calls: decode every argument `Term`
/// before the first result allocation.
fn ok_result_term(ctx: &mut ProcessContext, value: &str) -> Option<Term> {
    let value_term = ctx.alloc_binary(value.as_bytes()).ok()?;
    ctx.alloc_tuple(&[Term::atom(Atom::OK), value_term]).ok()
}

/// Build `{error, <<message>>}` on the calling process heap (see
/// [`ok_result_term`] for the allocation contract).
fn error_result_term(ctx: &mut ProcessContext, message: &str) -> Option<Term> {
    let value_term = ctx.alloc_binary(message.as_bytes()).ok()?;
    ctx.alloc_tuple(&[Term::atom(Atom::ERROR), value_term]).ok()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{
        Event, EventEnvelope, Payload, RunId, WorkflowError, WorkflowId, WorkflowStatus,
    };
    use aion_package::ContentHash;
    use aion_store::visibility::VisibilityStore;
    use aion_store::{EventStore, InMemoryStore, WriteToken};
    use serde_json::json;

    use super::{AwaitChildStep, await_child_step, next_child_key};
    use crate::durability::{CorrelationKey, Recorder};
    use crate::loader::WorkflowCatalog;
    use crate::registry::{
        CompletionNotifier, HandleResidency, Registry, WorkflowHandle, WorkflowHandleParts,
    };
    use crate::runtime::nif_child_engine::{ChildNifBridge, ChildNifBridgeParts};
    use crate::runtime::nif_context::NifContext;
    use crate::runtime::nif_state::{EngineNifState, PendingAwait};
    use crate::runtime::nif_timeout::TimeoutScope;
    use crate::runtime::{RuntimeConfig, RuntimeHandle, SignalDeliveryConfig};
    use crate::signal::SignalResumeHandoff;
    use crate::supervision::SupervisionTree;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Everything one `await_child_step` test needs over a synthesized
    /// parent history.
    struct AwaitHarness {
        state: Arc<EngineNifState>,
        bridge: Arc<ChildNifBridge>,
        runtime: Arc<RuntimeHandle>,
        store: Arc<dyn EventStore>,
        pid: u64,
        child_workflow_id: WorkflowId,
    }

    impl AwaitHarness {
        async fn over_parent_history(
            pid: u64,
            child_workflow_id: &WorkflowId,
            extra_events: &[Event],
        ) -> Result<Self, Box<dyn std::error::Error>> {
            let backing = Arc::new(InMemoryStore::default());
            let store: Arc<dyn EventStore> = Arc::clone(&backing) as Arc<dyn EventStore>;
            let visibility_store: Arc<dyn VisibilityStore> = backing;
            Self::with_stores(
                pid,
                child_workflow_id,
                extra_events,
                store,
                visibility_store,
            )
            .await
        }

        async fn with_stores(
            pid: u64,
            child_workflow_id: &WorkflowId,
            extra_events: &[Event],
            store: Arc<dyn EventStore>,
            visibility_store: Arc<dyn VisibilityStore>,
        ) -> Result<Self, Box<dyn std::error::Error>> {
            let registry = Arc::new(Registry::default());
            let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
            let workflow_id = WorkflowId::new_v4();
            let run_id = RunId::new_v4();
            let mut events = vec![started_event(&workflow_id, &run_id)?];
            events.extend_from_slice(extra_events);
            let head = events.len() as u64;
            store
                .append(WriteToken::recorder(), &workflow_id, &events, 0)
                .await?;
            let recorder = Recorder::resume_at(workflow_id.clone(), Arc::clone(&store), head);
            let handle = WorkflowHandle::new(WorkflowHandleParts {
                workflow_id: workflow_id.clone(),
                run_id: run_id.clone(),
                pid,
                workflow_type: "parent".to_owned(),
                namespace: String::from("default"),
                loaded_version: ContentHash::from_bytes([3; 32]),
                cached_status: WorkflowStatus::Running,
                residency: HandleResidency::Resident,
                recorder,
                completion: CompletionNotifier::new(),
            });
            registry.insert((workflow_id, run_id), handle)?;
            let bridge = Arc::new(ChildNifBridge::new(ChildNifBridgeParts {
                store: Arc::clone(&store),
                visibility_store,
                runtime: Arc::clone(&runtime),
                catalog: Arc::new(WorkflowCatalog::new()),
                registry,
                supervision: Arc::new(SupervisionTree::new()),
                signal_handoff: Arc::new(SignalResumeHandoff::new()),
                search_attribute_schema: Arc::new(aion_core::SearchAttributeSchema::new()),
                tokio_handle: tokio::runtime::Handle::current(),
                watch_backoff: SignalDeliveryConfig::default(),
            })?);
            Ok(Self {
                state: Arc::new(EngineNifState::default()),
                bridge,
                runtime,
                store,
                pid,
                child_workflow_id: child_workflow_id.clone(),
            })
        }

        fn step(&self) -> Result<AwaitChildStep, String> {
            // Production runs this on a beamr scheduler thread with no
            // ambient Tokio context; block_in_place mirrors that so the
            // step's history reads can block_on the harness runtime.
            tokio::task::block_in_place(|| {
                await_child_step(&self.state, &self.bridge, self.pid, &self.child_workflow_id)
            })
        }

        fn pinned_child(&self) -> Option<WorkflowId> {
            match self.state.pending_awaits.get(&self.pid).map(|e| e.clone()) {
                Some(PendingAwait::Child { child_workflow_id }) => Some(child_workflow_id),
                _ => None,
            }
        }

        fn shutdown(self) -> TestResult {
            self.bridge.shutdown_child_tasks();
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

    fn child_started(workflow_id: &WorkflowId, child_workflow_id: &WorkflowId, seq: u64) -> Event {
        Event::ChildWorkflowStarted {
            envelope: envelope(workflow_id, seq),
            child_workflow_id: child_workflow_id.clone(),
            workflow_type: "child".to_owned(),
            input: Payload::new(aion_core::ContentType::Json, br#""child-input""#.to_vec()),
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        }
    }

    fn queue_query(
        state: &EngineNifState,
        pid: u64,
        query_id: &str,
    ) -> Result<tokio::sync::oneshot::Receiver<crate::query::QueryResult>, Box<dyn std::error::Error>>
    {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        crate::runtime::nif_query::insert_pending_reply(state, query_id.to_owned(), pid, sender)?;
        state.pending_queries.entry(pid).or_default().push_back(
            crate::runtime::nif_state::PendingQuery {
                query_id: query_id.to_owned(),
                name: "state".to_owned(),
            },
        );
        Ok(receiver)
    }

    fn started_event(workflow_id: &WorkflowId, run_id: &RunId) -> TestEvent {
        Ok(Event::WorkflowStarted {
            envelope: EventEnvelope {
                seq: 1,
                recorded_at: chrono::Utc::now(),
                workflow_id: workflow_id.clone(),
            },
            workflow_type: "parent".to_owned(),
            input: Payload::from_json(&json!({ "fixture": "input" }))?,
            run_id: run_id.clone(),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        })
    }

    type TestEvent = Result<Event, Box<dyn std::error::Error>>;

    fn registered_context(
        runtime: &tokio::runtime::Runtime,
        pid: u64,
        resume_head: u64,
    ) -> Result<(Registry, Arc<dyn EventStore>), Box<dyn std::error::Error>> {
        let registry = Registry::default();
        let workflow_id = WorkflowId::new_v4();
        let run_id = RunId::new_v4();
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        runtime.block_on(store.append(
            WriteToken::recorder(),
            &workflow_id,
            &[started_event(&workflow_id, &run_id)?],
            0,
        ))?;
        let recorder = Recorder::resume_at(workflow_id.clone(), Arc::clone(&store), resume_head);
        let handle = WorkflowHandle::new(WorkflowHandleParts {
            workflow_id: workflow_id.clone(),
            run_id: run_id.clone(),
            pid,
            workflow_type: "parent".to_owned(),
            namespace: String::from("default"),
            loaded_version: ContentHash::from_bytes([3; 32]),
            cached_status: WorkflowStatus::Running,
            residency: HandleResidency::Resident,
            recorder,
            completion: CompletionNotifier::new(),
        });
        registry.insert((workflow_id, run_id), handle)?;
        Ok((registry, store))
    }

    #[test]
    fn child_keys_are_run_scoped_ordinals_independent_of_recorder_head() -> TestResult {
        let runtime = tokio::runtime::Runtime::new()?;
        // A recovered run resumes its recorder at the full history head; the
        // spawn key must still start at ordinal zero, not head + 1.
        let (registry, store) = registered_context(&runtime, 91, 57)?;
        let first_call = NifContext::new_with_history_store(
            91,
            &registry,
            runtime.handle().clone(),
            Some(Arc::clone(&store)),
            SignalDeliveryConfig::default(),
        )?;
        let second_call = NifContext::new_with_history_store(
            91,
            &registry,
            runtime.handle().clone(),
            Some(store),
            SignalDeliveryConfig::default(),
        )?;

        assert_eq!(next_child_key(&first_call), CorrelationKey::Child(0));
        // Distinct NIF calls share the handle-owned counter, so the second
        // spawn in the same run advances to the next ordinal.
        assert_eq!(next_child_key(&second_call), CorrelationKey::Child(1));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn live_await_pins_arms_watcher_and_suspends_idempotently() -> TestResult {
        let child = WorkflowId::new_v4();
        let parent_id = WorkflowId::new_v4();
        let harness =
            AwaitHarness::over_parent_history(301, &child, &[child_started(&parent_id, &child, 2)])
                .await?;

        // First live arrival: pin set, watcher armed, suspend requested.
        assert_eq!(harness.step(), Ok(AwaitChildStep::Suspend));
        assert_eq!(harness.pinned_child(), Some(child.clone()));
        assert_eq!(harness.bridge.child_tasks().armed_watch_count(), 1);

        // Wake re-entry with nothing recorded: same pin, no second watcher.
        assert_eq!(harness.step(), Ok(AwaitChildStep::Suspend));
        assert_eq!(harness.pinned_child(), Some(child));
        assert_eq!(harness.bridge.child_tasks().armed_watch_count(), 1);
        harness.shutdown()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_sentinel_precedes_recorded_resolution_and_preserves_pin() -> TestResult {
        let child = WorkflowId::new_v4();
        let parent_id = WorkflowId::new_v4();
        // The child terminal is already recorded: without a pending query
        // this step would resolve immediately — the sentinel must still win.
        let harness = AwaitHarness::over_parent_history(
            302,
            &child,
            &[
                child_started(&parent_id, &child, 2),
                Event::ChildWorkflowCompleted {
                    envelope: envelope(&parent_id, 3),
                    child_workflow_id: child.clone(),
                    result: Payload::from_json(&json!(42))?,
                },
            ],
        )
        .await?;
        harness.state.pending_awaits.insert(
            harness.pid,
            PendingAwait::Child {
                child_workflow_id: child.clone(),
            },
        );
        let _receiver = queue_query(&harness.state, harness.pid, "q-await-1")?;

        let step = harness.step();

        match step {
            Ok(AwaitChildStep::QuerySentinel(sentinel)) => {
                assert!(sentinel.starts_with("aion_query:"), "sentinel: {sentinel}");
                assert!(sentinel.contains("q-await-1"));
            }
            other => return Err(format!("expected the query sentinel, got {other:?}").into()),
        }
        // The pin is untouched: the pump re-enters the same logical await.
        assert_eq!(harness.pinned_child(), Some(child.clone()));

        // After the pump replies (servicing flag cleared), the re-entry
        // resolves the recorded terminal and clears the pin.
        crate::runtime::nif_query_pump::clear_servicing_query(
            &harness.state,
            harness.pid,
            "q-await-1",
        );
        assert_eq!(
            harness.step(),
            Ok(AwaitChildStep::ChildResolved("ok:42".to_owned()))
        );
        assert_eq!(harness.pinned_child(), None);
        harness.shutdown()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recorded_failure_resolves_as_error_prefixed_data_and_clears_pin() -> TestResult {
        let child = WorkflowId::new_v4();
        let parent_id = WorkflowId::new_v4();
        let harness = AwaitHarness::over_parent_history(
            303,
            &child,
            &[
                child_started(&parent_id, &child, 2),
                Event::ChildWorkflowFailed {
                    envelope: envelope(&parent_id, 3),
                    child_workflow_id: child.clone(),
                    error: WorkflowError {
                        message: "cancelled:operator".to_owned(),
                        details: None,
                    },
                },
            ],
        )
        .await?;
        harness.state.pending_awaits.insert(
            harness.pid,
            PendingAwait::Child {
                child_workflow_id: child.clone(),
            },
        );

        // D4 envelope: child failure is `{ok, "error:..."}` data, never an
        // engine fault.
        assert_eq!(
            harness.step(),
            Ok(AwaitChildStep::ChildResolved(
                "error:cancelled:operator".to_owned()
            ))
        );
        assert_eq!(harness.pinned_child(), None);
        harness.shutdown()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn expired_scope_releases_pin_without_touching_the_child() -> TestResult {
        let child = WorkflowId::new_v4();
        let parent_id = WorkflowId::new_v4();
        let harness =
            AwaitHarness::over_parent_history(304, &child, &[child_started(&parent_id, &child, 2)])
                .await?;
        harness.state.pending_awaits.insert(
            harness.pid,
            PendingAwait::Child {
                child_workflow_id: child.clone(),
            },
        );
        harness
            .state
            .timeout_scopes
            .insert(7, TimeoutScope::replayed_for_test(harness.pid, true));
        harness
            .state
            .timeout_scope_stacks
            .insert(harness.pid, vec![7]);
        let parent_history_before = harness.store.read_history(&parent_id).await?;

        let step = harness.step();

        // D-1: abort-the-await, leave-the-child-running. The typed scope
        // error is returned, the pin is released, no watcher is armed, and
        // nothing is recorded anywhere.
        assert_eq!(
            step,
            Ok(AwaitChildStep::ScopeExpired(
                "timeout:deadline expired".to_owned()
            ))
        );
        assert_eq!(harness.pinned_child(), None);
        assert_eq!(harness.bridge.child_tasks().armed_watch_count(), 0);
        assert_eq!(
            harness.store.read_history(&parent_id).await?,
            parent_history_before
        );
        assert!(harness.store.read_history(&child).await?.is_empty());
        harness.shutdown()
    }

    /// F1a: a scope expiring after the watcher was armed must disarm it —
    /// otherwise the watcher records the child terminal into a history the
    /// live run already branched away from, and replay resolves the await
    /// against an arrival live never observed. Before the fix the watcher
    /// stayed armed (count 1) through the expired-scope abort.
    #[tokio::test(flavor = "multi_thread")]
    async fn expired_scope_disarms_the_armed_watcher() -> TestResult {
        let child = WorkflowId::new_v4();
        let parent_id = WorkflowId::new_v4();
        let harness =
            AwaitHarness::over_parent_history(307, &child, &[child_started(&parent_id, &child, 2)])
                .await?;

        // Live first arrival: the watcher arms and the await parks.
        assert_eq!(harness.step(), Ok(AwaitChildStep::Suspend));
        assert_eq!(harness.bridge.child_tasks().armed_watch_count(), 1);

        // The enclosing with_timeout deadline expires before any terminal.
        harness
            .state
            .timeout_scopes
            .insert(9, TimeoutScope::replayed_for_test(harness.pid, true));
        harness
            .state
            .timeout_scope_stacks
            .insert(harness.pid, vec![9]);

        assert_eq!(
            harness.step(),
            Ok(AwaitChildStep::ScopeExpired(
                "timeout:deadline expired".to_owned()
            ))
        );
        assert_eq!(harness.pinned_child(), None);
        assert_eq!(
            harness.bridge.child_tasks().armed_watch_count(),
            0,
            "the expired-scope abort must disarm the await's watcher (F1a)"
        );
        harness.shutdown()
    }

    /// F1b: when both the scope deadline's `TimerFired` and the child
    /// terminal are recorded, history order decides the branch — a deadline
    /// recorded first means the live run timed out before the watcher's
    /// record landed, so resolution takes the timeout branch on live and
    /// replay alike. Before the fix this resolved `ChildResolved("ok:42")`.
    #[tokio::test(flavor = "multi_thread")]
    async fn deadline_recorded_before_child_terminal_takes_the_timeout_branch() -> TestResult {
        let child = WorkflowId::new_v4();
        let parent_id = WorkflowId::new_v4();
        let harness = AwaitHarness::over_parent_history(
            308,
            &child,
            &[
                child_started(&parent_id, &child, 2),
                Event::TimerFired {
                    envelope: envelope(&parent_id, 3),
                    timer_id: aion_core::TimerId::anonymous(0),
                },
                Event::ChildWorkflowCompleted {
                    envelope: envelope(&parent_id, 4),
                    child_workflow_id: child.clone(),
                    result: Payload::from_json(&json!(42))?,
                },
            ],
        )
        .await?;
        harness.state.pending_awaits.insert(
            harness.pid,
            PendingAwait::Child {
                child_workflow_id: child.clone(),
            },
        );
        harness
            .state
            .timeout_scopes
            .insert(11, TimeoutScope::replayed_for_test(harness.pid, true));
        harness
            .state
            .timeout_scope_stacks
            .insert(harness.pid, vec![11]);

        assert_eq!(
            harness.step(),
            Ok(AwaitChildStep::ScopeExpired(
                "timeout:deadline expired".to_owned()
            )),
            "a deadline recorded before the child terminal must win (F1b)"
        );
        assert_eq!(harness.pinned_child(), None);
        harness.shutdown()
    }

    /// F1b converse: a child terminal recorded *before* the deadline fired
    /// was observed by the live run, so both live and replay resolve the
    /// child's outcome even though the scope is expired by resolution time.
    #[tokio::test(flavor = "multi_thread")]
    async fn child_terminal_recorded_before_deadline_resolves_the_child() -> TestResult {
        let child = WorkflowId::new_v4();
        let parent_id = WorkflowId::new_v4();
        let harness = AwaitHarness::over_parent_history(
            309,
            &child,
            &[
                child_started(&parent_id, &child, 2),
                Event::ChildWorkflowCompleted {
                    envelope: envelope(&parent_id, 3),
                    child_workflow_id: child.clone(),
                    result: Payload::from_json(&json!(42))?,
                },
                Event::TimerFired {
                    envelope: envelope(&parent_id, 4),
                    timer_id: aion_core::TimerId::anonymous(0),
                },
            ],
        )
        .await?;
        harness.state.pending_awaits.insert(
            harness.pid,
            PendingAwait::Child {
                child_workflow_id: child.clone(),
            },
        );
        harness
            .state
            .timeout_scopes
            .insert(13, TimeoutScope::replayed_for_test(harness.pid, true));
        harness
            .state
            .timeout_scope_stacks
            .insert(harness.pid, vec![13]);

        assert_eq!(
            harness.step(),
            Ok(AwaitChildStep::ChildResolved("ok:42".to_owned())),
            "a child terminal recorded before the deadline resolves the child"
        );
        assert_eq!(harness.pinned_child(), None);
        harness.shutdown()
    }

    /// N-1: the `ResumeLive` expiry decision must be a pure function of the
    /// resolution snapshot. Race modeled: the await's resolution read (H1)
    /// has neither the child terminal nor the deadline `TimerFired`; both
    /// land before any later read — child terminal (seq 3) BEFORE the
    /// deadline `TimerFired` (seq 4). Before the fix the live path asked
    /// `expired_scope_message` (a FRESH store read via the timer bridge),
    /// saw the fired deadline, and took the timeout branch — while replay
    /// resolved `Recorded(ChildCompleted)` and F1b (4 < 3 is false) took the
    /// child branch: opposite branches live vs replay. After the fix the
    /// stale-snapshot step suspends and the next wake converges on the
    /// child branch, byte-identical to replay.
    #[tokio::test(flavor = "multi_thread")]
    async fn stale_snapshot_live_timeout_converges_with_replay_child_branch() -> TestResult {
        let child = WorkflowId::new_v4();
        let parent_id = WorkflowId::new_v4();
        let scope_timer = aion_core::TimerId::anonymous(7);
        // H1 = WorkflowStarted + ChildWorkflowStarted only.
        let backing = Arc::new(crate::runtime::nif_test_stores::StaleReadStore::new(2));
        let store: Arc<dyn EventStore> = Arc::clone(&backing) as Arc<dyn EventStore>;
        let visibility: Arc<dyn VisibilityStore> = Arc::new(InMemoryStore::default());
        let harness = AwaitHarness::with_stores(
            311,
            &child,
            &[
                child_started(&parent_id, &child, 2),
                Event::ChildWorkflowCompleted {
                    envelope: envelope(&parent_id, 3),
                    child_workflow_id: child.clone(),
                    result: Payload::from_json(&json!(42))?,
                },
                Event::TimerFired {
                    envelope: envelope(&parent_id, 4),
                    timer_id: scope_timer.clone(),
                },
            ],
            store,
            visibility,
        )
        .await?;
        // The harness minted the parent id; find it through the registry and
        // arm exactly one stale read for it.
        let registered_parent = harness
            .bridge
            .registry()
            .list()?
            .into_iter()
            .next()
            .ok_or("no registered parent")?
            .workflow_id()
            .clone();
        backing.set_stale_target(&registered_parent, 1);

        // The timer bridge backs the OLD fresh-read path; installing it
        // proves this test fails pre-fix instead of accidentally passing
        // because the fresh read was unavailable.
        crate::runtime::nif_timer_bridge::install_timer_nif_bridge(
            &harness.state,
            harness.bridge.registry_arc(),
            harness.bridge.store(),
            tokio::runtime::Handle::current(),
            SignalDeliveryConfig::default(),
        );
        // Live scope whose deadline is the recorded TimerFired(seq 4).
        harness
            .state
            .timeout_scopes
            .insert(21, TimeoutScope::live_for_test(harness.pid, scope_timer));
        harness
            .state
            .timeout_scope_stacks
            .insert(harness.pid, vec![21]);

        // Step 1 — stale resolution snapshot (neither event): must suspend,
        // never decide the branch from a fresh read.
        assert_eq!(
            harness.step(),
            Ok(AwaitChildStep::Suspend),
            "a snapshot lacking both events must park, not branch (N-1)"
        );

        // Step 2 — fresh snapshot: F1b orders child terminal (3) before the
        // deadline (4) and resolves the child branch.
        assert_eq!(
            harness.step(),
            Ok(AwaitChildStep::ChildResolved("ok:42".to_owned()))
        );
        assert_eq!(harness.pinned_child(), None);

        // Replay equivalence: a fresh engine state derives the scope outcome
        // from history (replay-expired) and must take the same child branch.
        let replay_state = Arc::new(EngineNifState::default());
        replay_state.timeout_scopes.insert(
            1,
            TimeoutScope::replayed_expired_with_deadline_for_test(
                harness.pid,
                aion_core::TimerId::anonymous(7),
            ),
        );
        replay_state
            .timeout_scope_stacks
            .insert(harness.pid, vec![1]);
        let replayed = tokio::task::block_in_place(|| {
            await_child_step(
                &replay_state,
                &harness.bridge,
                harness.pid,
                &harness.child_workflow_id,
            )
        });
        assert_eq!(
            replayed,
            Ok(AwaitChildStep::ChildResolved("ok:42".to_owned())),
            "replay must take the same branch as the converged live run"
        );
        harness.shutdown()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn mismatched_pin_is_a_typed_error() -> TestResult {
        let child = WorkflowId::new_v4();
        let parent_id = WorkflowId::new_v4();
        let harness =
            AwaitHarness::over_parent_history(305, &child, &[child_started(&parent_id, &child, 2)])
                .await?;

        // A different pinned child id is a hard error.
        let other = WorkflowId::new_v4();
        harness.state.pending_awaits.insert(
            harness.pid,
            PendingAwait::Child {
                child_workflow_id: other.clone(),
            },
        );
        let error = harness.step().err().ok_or("mismatched pin was accepted")?;
        assert!(error.contains(&other.to_string()), "error: {error}");

        // A different pinned await kind is a hard error too.
        harness
            .state
            .pending_awaits
            .insert(harness.pid, PendingAwait::Signal { index: 0 });
        let error = harness.step().err().ok_or("signal pin was accepted")?;
        assert!(error.contains("pending signal await"), "error: {error}");
        harness.shutdown()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn awaiting_a_child_the_engine_has_no_trace_of_is_a_typed_error() -> TestResult {
        // The parent's run segment has no ChildWorkflowStarted for the id,
        // the registry has no handle, and no child history exists: parking
        // would wedge the caller forever, so the await refuses typed.
        let child = WorkflowId::new_v4();
        let harness = AwaitHarness::over_parent_history(306, &child, &[]).await?;

        let error = harness.step().err().ok_or("unknown child was accepted")?;

        assert!(
            error.contains(&format!("unknown_child_workflow:{child}")),
            "error: {error}"
        );
        assert_eq!(harness.pinned_child(), None);
        assert_eq!(harness.bridge.child_tasks().armed_watch_count(), 0);
        harness.shutdown()
    }
}
