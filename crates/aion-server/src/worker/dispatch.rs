//! Push dispatch for remote activity workers and result handoff to the engine contract.

use std::collections::BTreeMap;

use aion_core::{ActivityError, ActivityErrorKind, ActivityId, Payload, RunId, WorkflowId};
use aion_proto::{
    ProtoActivityId, ProtoActivityResult, ProtoActivityTask, ProtoPayload, ProtoRunId,
    ProtoWorkflowId, WireError, proto_activity_result,
};

use crate::error::ServerError;
use crate::shutdown::DrainState;
use crate::worker::registry::{ConnectedWorkerRegistry, WorkerMessage};
use tracing::{Instrument, info_span};

/// Scheduled remote activity that must be placed with a connected worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledActivity {
    /// Namespace selected by the adapter boundary before dispatch — the
    /// correctness/isolation boundary the activity may dispatch within.
    pub namespace: String,
    /// Task queue (pool/flavour) selected within the namespace. The worker-pool
    /// address is `(namespace, task_queue)`; an empty value is normalized to the
    /// named default pool by the registry lookup.
    pub task_queue: String,
    /// Activity type to match against worker registrations, *within* the
    /// selected pool.
    pub activity_type: String,
    /// Optional node locality affinity. `Some(node)` pins this dispatch to
    /// workers advertising that node (require semantics: it waits if none are
    /// present, exactly like the no-worker path); `None` is unpinned and reaches
    /// any worker in the `(namespace, task_queue)` pool — byte-identical to the
    /// pre-NODE behaviour. Producers stamp `None` until SDK selection (NODE-4)
    /// and the durable column (NODE-2) land.
    pub node: Option<String>,
    /// Owning workflow id.
    pub workflow_id: WorkflowId,
    /// Correlating activity id.
    pub activity_id: ActivityId,
    /// Concrete workflow run that staged this task, when known.
    pub run_id: Option<RunId>,
    /// Opaque activity input payload.
    pub input: Payload,
    /// One-based delivery attempt stamped by the dispatching engine seam.
    /// Zero is malformed on the wire; producers must always stamp it.
    pub attempt: u32,
    /// Display labels the workflow attached to the activity. Display metadata
    /// only — carried to the worker for its logs and the dashboard.
    pub labels: BTreeMap<String, String>,
}

impl ScheduledActivity {
    /// Build the wire task pushed to the worker stream.
    #[must_use]
    pub fn to_task(&self) -> ProtoActivityTask {
        ProtoActivityTask {
            workflow_id: Some(ProtoWorkflowId::from(self.workflow_id.clone())),
            activity_id: Some(ProtoActivityId::from(self.activity_id.clone())),
            activity_type: self.activity_type.clone(),
            input: Some(ProtoPayload::from(self.input.clone())),
            attempt: self.attempt,
            labels: self.labels.clone().into_iter().collect(),
            run_id: self.run_id.clone().map(ProtoRunId::from),
        }
    }
}

/// Push dispatcher backed by the connected-worker registry.
#[derive(Clone, Debug)]
pub struct ActivityDispatcher {
    registry: ConnectedWorkerRegistry,
    drain_state: DrainState,
}

impl ActivityDispatcher {
    /// Build a dispatcher over the shared worker registry.
    #[must_use]
    pub fn new(registry: ConnectedWorkerRegistry) -> Self {
        Self {
            registry,
            drain_state: DrainState::default(),
        }
    }

    /// Share the server drain gate.
    #[must_use]
    pub fn with_drain_state(mut self, drain_state: DrainState) -> Self {
        self.drain_state = drain_state;
        self
    }

    /// Push a scheduled activity to a matching worker.
    ///
    /// # Errors
    ///
    /// Returns a typed dispatch error if no worker is available or the selected
    /// stream is closed; returns lock poison if registry access cannot be trusted.
    pub async fn dispatch(&self, activity: &ScheduledActivity) -> Result<(), ServerError> {
        let span = info_span!(
            "activity_dispatch",
            operation = "activity_dispatch",
            namespace = %activity.namespace,
            task_queue = %activity.task_queue,
            node = activity.node.as_deref(),
            workflow_id = %activity.workflow_id,
            activity_id = %activity.activity_id,
            activity_type = %activity.activity_type,
            worker_id = tracing::field::Empty,
        );
        let span_fields = span.clone();

        async {
            self.dispatch_to_node(activity, activity.node.as_deref(), &span_fields)
                .await
        }
        .instrument(span)
        .await
        .inspect_err(|error| {
            log_dispatch_error("activity_dispatch", activity, error);
        })
    }

    /// Dispatch `activity` preferring workers on one of the `preferred` node
    /// labels, spilling to ANY live worker when none of the preferred labels has a
    /// live worker (Control-Plane Phase 2, P2-P3 — the `Prefer{L}` soft spill).
    ///
    /// This is consulted ONLY for an UNPINNED activity (`activity.node == None`):
    /// a per-activity authored pin always wins and is dispatched through
    /// [`Self::dispatch`] unchanged. The recorded row's `node` is NEVER mutated —
    /// preference is a pure dispatch-time worker-selection optimization in this
    /// non-replayed path, exactly like the existing round-robin, so replay is
    /// untouched (CP-Phase-2 §2.4).
    ///
    /// The prefer-then-spill tier sequence is derived ONCE, from the shared
    /// [`preferred_node_order`](crate::worker::preferred_node_order), so this gRPC
    /// path and the liminal
    /// [`RegistryLiminalDispatch`](crate::worker::RegistryLiminalDispatch) can never
    /// diverge on what "prefer labelled worker, spill to any" means:
    ///
    /// Tier 1..N: for each preferred label (deterministic set order) try a
    /// NON-WAITING `workers_for(node = Some(label))` and dispatch to the first
    /// live worker found. Tier N+1 (spill): if no preferred label has a live
    /// worker, fall back to [`Self::dispatch`] with the activity's own (unpinned)
    /// node, so the wait-for-worker backstop and round-robin behave exactly as
    /// today. An empty `preferred` set is the spill case immediately.
    ///
    /// # Errors
    ///
    /// As [`Self::dispatch`].
    pub async fn dispatch_preferring(
        &self,
        activity: &ScheduledActivity,
        preferred: &std::collections::BTreeSet<String>,
    ) -> Result<(), ServerError> {
        // Reconstruct the shared tier order from the preferred labels so gRPC and
        // liminal consult ONE prefer-then-spill implementation.
        let tiers = crate::worker::preferred_node_order(&aion_store::NamespacePlacement::Prefer {
            nodes: preferred.clone(),
        });
        self.dispatch_over_tiers(activity, &tiers).await
    }

    /// Dispatch `activity` REQUIRING a worker whose advertised node is one of the
    /// `required` labels, WAITING when none is live and NEVER spilling to a
    /// node=`None` any-worker dispatch (Control-Plane Phase 2, P2-I1 — the
    /// `Pinned{L}` hard pin). This is the opposite of [`Self::dispatch_preferring`]:
    /// a `Prefer` set appends a `None` spill tier; a `Pinned` set has NO `None`
    /// tier and instead holds on the wait-for-worker backstop until an L-labelled
    /// worker registers.
    ///
    /// Consulted ONLY for an UNPINNED activity (`activity.node == None`): a
    /// per-activity authored pin always wins and dispatches through
    /// [`Self::dispatch`] unchanged. The recorded row's `node` is NEVER mutated —
    /// the required set is a pure dispatch-time worker-selection input in this
    /// non-replayed path, so replay is untouched (CP-Phase-2 §2.4).
    ///
    /// Each retry tries every required label (deterministic [`BTreeSet`] order) via
    /// a NON-WAITING `workers_for(node = Some(label))` and delivers to the first
    /// live worker found, preserving the round-robin exactly like
    /// [`Self::dispatch_to_node`]. When no required label has a live worker across
    /// the whole set, it awaits [`wait_for_worker`](crate::worker::ConnectedWorkerRegistry::wait_for_worker)
    /// and retries — the same isolation-stall a per-activity `Some(N)` pin already
    /// exhibits. An EMPTY required set can never be satisfied by any labelled
    /// worker, so it stalls (isolation > availability); the caller sets a non-empty
    /// `Pinned{L}` for a live pin.
    ///
    /// # Errors
    ///
    /// As [`Self::dispatch`].
    pub async fn dispatch_requiring(
        &self,
        activity: &ScheduledActivity,
        required: &std::collections::BTreeSet<String>,
    ) -> Result<(), ServerError> {
        let span = info_span!(
            "activity_dispatch",
            operation = "activity_dispatch_requiring",
            namespace = %activity.namespace,
            task_queue = %activity.task_queue,
            workflow_id = %activity.workflow_id,
            activity_id = %activity.activity_id,
            activity_type = %activity.activity_type,
            worker_id = tracing::field::Empty,
        );
        let span_fields = span.clone();
        async {
            loop {
                for label in required {
                    self.drain_state
                        .ensure_accepting(&activity.namespace, &activity.activity_type)?;
                    let candidates = self.registry.workers_for(
                        &activity.namespace,
                        &activity.task_queue,
                        &activity.activity_type,
                        Some(label.as_str()),
                    )?;
                    if let Some(()) = self
                        .send_to_candidates(activity, candidates, &span_fields)
                        .await?
                    {
                        return Ok(());
                    }
                }
                // No required label had a live worker this pass. WAIT for a worker
                // to register, then retry the WHOLE required set — never fall back
                // to a node=None any-worker dispatch (the hard-pin invariant).
                tracing::info!(
                    namespace = %activity.namespace,
                    task_queue = %activity.task_queue,
                    activity_type = %activity.activity_type,
                    workflow_id = %activity.workflow_id,
                    activity_id = %activity.activity_id,
                    "no worker on a required (Pinned) node; waiting — will NOT spill to any-node"
                );
                self.registry.wait_for_worker().await;
            }
        }
        .instrument(span)
        .await
        .inspect_err(|error| {
            log_dispatch_error("activity_dispatch_requiring", activity, error);
        })
    }

    /// Dispatch `activity` over an ordered `tiers` sequence of node filters, each
    /// a `Some(label)` preference or the final `None` spill (the shared
    /// [`preferred_node_order`](crate::worker::preferred_node_order) output). The
    /// first non-spill tier with a live worker wins via a NON-WAITING
    /// `workers_for`; the `None` spill tier falls back to the waiting
    /// [`Self::dispatch_to_node`] so the wait-for-worker backstop and round-robin
    /// behave exactly as today.
    ///
    /// # Errors
    ///
    /// As [`Self::dispatch`].
    async fn dispatch_over_tiers(
        &self,
        activity: &ScheduledActivity,
        tiers: &[Option<String>],
    ) -> Result<(), ServerError> {
        let span = info_span!(
            "activity_dispatch",
            operation = "activity_dispatch_preferring",
            namespace = %activity.namespace,
            task_queue = %activity.task_queue,
            workflow_id = %activity.workflow_id,
            activity_id = %activity.activity_id,
            activity_type = %activity.activity_type,
            worker_id = tracing::field::Empty,
        );
        let span_fields = span.clone();
        async {
            for tier in tiers {
                let Some(label) = tier else {
                    // The `None` spill tier: fall back to the waiting unpinned
                    // dispatch (wait-for-worker backstop + round-robin).
                    return self
                        .dispatch_to_node(activity, activity.node.as_deref(), &span_fields)
                        .await;
                };
                self.drain_state
                    .ensure_accepting(&activity.namespace, &activity.activity_type)?;
                let candidates = self.registry.workers_for(
                    &activity.namespace,
                    &activity.task_queue,
                    &activity.activity_type,
                    Some(label.as_str()),
                )?;
                if let Some(()) = self
                    .send_to_candidates(activity, candidates, &span_fields)
                    .await?
                {
                    return Ok(());
                }
            }
            // An empty tier list (never produced by `preferred_node_order`, which
            // always appends the spill) still degrades to the unpinned dispatch.
            self.dispatch_to_node(activity, activity.node.as_deref(), &span_fields)
                .await
        }
        .instrument(span)
        .await
        .inspect_err(|error| {
            log_dispatch_error("activity_dispatch_preferring", activity, error);
        })
    }

    /// The waiting dispatch core: select a worker for `node` (waiting for one to
    /// register when none is live, exactly as before), then push the task.
    async fn dispatch_to_node(
        &self,
        activity: &ScheduledActivity,
        node: Option<&str>,
        span_fields: &tracing::Span,
    ) -> Result<(), ServerError> {
        let workers = loop {
            self.drain_state
                .ensure_accepting(&activity.namespace, &activity.activity_type)?;
            let candidates = self.registry.workers_for(
                &activity.namespace,
                &activity.task_queue,
                &activity.activity_type,
                node,
            )?;
            if !candidates.is_empty() {
                break candidates;
            }
            tracing::info!(
                namespace = %activity.namespace,
                task_queue = %activity.task_queue,
                node = node,
                activity_type = %activity.activity_type,
                workflow_id = %activity.workflow_id,
                activity_id = %activity.activity_id,
                "no connected worker; waiting for a matching worker to register"
            );
            self.registry.wait_for_worker().await;
        };
        match self
            .send_to_candidates(activity, workers, span_fields)
            .await?
        {
            Some(()) => Ok(()),
            None => Err(ServerError::worker_dispatch(
                activity.namespace.clone(),
                activity.activity_type.clone(),
                format!(
                    "all matching worker streams in task queue {} closed before task could be \
                     delivered",
                    activity.task_queue
                ),
            )),
        }
    }

    /// Try each candidate in order, pushing the task to the first live stream.
    /// Returns `Ok(Some(()))` on a delivered task, `Ok(None)` when every candidate
    /// stream was already closed (deregistered as it went). An empty candidate
    /// list returns `Ok(None)` so callers can treat it as "no live worker here".
    async fn send_to_candidates(
        &self,
        activity: &ScheduledActivity,
        candidates: Vec<crate::worker::registry::WorkerHandle>,
        span_fields: &tracing::Span,
    ) -> Result<Option<()>, ServerError> {
        for worker in candidates {
            self.drain_state
                .ensure_accepting(&activity.namespace, &activity.activity_type)?;
            span_fields.record("worker_id", format!("{:?}", worker.id()));
            // The gRPC dispatch path only registers gRPC-delivery workers, so a
            // worker here always carries a stream sender; a missing one means a
            // non-gRPC-transport worker leaked into this path and cannot be served
            // over it, so it is deregistered like a closed stream.
            if let Some(sender) = worker.sender() {
                if sender
                    .send(WorkerMessage::ActivityTask(activity.to_task()))
                    .await
                    .is_ok()
                {
                    return Ok(Some(()));
                }
            }
            self.registry.deregister(worker.id())?;
        }
        Ok(None)
    }
}

fn log_dispatch_error(operation: &'static str, activity: &ScheduledActivity, error: &ServerError) {
    let fields = error.trace_fields();
    tracing::error!(
        operation,
        namespace = %activity.namespace,
        task_queue = %activity.task_queue,
        node = activity.node.as_deref(),
        workflow_id = %activity.workflow_id,
        activity_id = %activity.activity_id,
        activity_type = %activity.activity_type,
        error_type = %fields.error_type,
        store_error_type = fields.store_error_type,
        reason = %fields.reason,
        "activity dispatch failed"
    );
}

/// Decoded activity outcome reported by a worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActivityCompletionOutcome {
    /// Activity completed successfully with an output payload.
    Succeeded(Payload),
    /// Activity failed, preserving retryability classification for the engine.
    Failed(ActivityError),
}

/// Correlated activity completion handed to the engine-owned activity contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivityCompletion {
    /// Owning workflow id.
    pub workflow_id: WorkflowId,
    /// Correlating activity id.
    pub activity_id: ActivityId,
    /// Concrete workflow run echoed by the worker, when known.
    pub run_id: Option<RunId>,
    /// Worker-reported outcome.
    pub outcome: ActivityCompletionOutcome,
}

impl TryFrom<ProtoActivityResult> for ActivityCompletion {
    type Error = ServerError;

    fn try_from(value: ProtoActivityResult) -> Result<Self, Self::Error> {
        let workflow_id = value
            .workflow_id
            .ok_or_else(|| wire_error("activity result workflow id is missing"))
            .and_then(|id| WorkflowId::try_from(id).map_err(ServerError::from))?;
        let activity_id = value
            .activity_id
            .ok_or_else(|| wire_error("activity result activity id is missing"))
            .map(ActivityId::from)?;
        let run_id = value
            .run_id
            .map(|id| RunId::try_from(id).map_err(ServerError::from))
            .transpose()?;
        let outcome = match value.outcome {
            Some(proto_activity_result::Outcome::Result(payload)) => {
                ActivityCompletionOutcome::Succeeded(
                    Payload::try_from(payload).map_err(ServerError::from)?,
                )
            }
            Some(proto_activity_result::Outcome::Error(error)) => {
                ActivityCompletionOutcome::Failed(
                    ActivityError::try_from(error).map_err(ServerError::from)?,
                )
            }
            None => return Err(wire_error("activity result outcome is missing")),
        };

        Ok(Self {
            workflow_id,
            activity_id,
            run_id,
            outcome,
        })
    }
}

/// Engine-owned activity completion contract used by the worker endpoint.
pub trait ActivityCompletionSink {
    /// Feed one worker-reported result into the engine activity contract.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] when the engine rejects or cannot record the completion.
    fn complete_activity(&self, completion: ActivityCompletion) -> Result<(), ServerError>;

    /// Park one in-flight dispatch for restart recovery during a graceful
    /// drain (#207): resolve the LOCAL waiter with the ephemeral parked
    /// sentinel and nothing else.
    ///
    /// Parking is the anti-completion — it writes nothing durable, delivers
    /// nothing to workflow code, and never crosses the SDK wire. It exists so a
    /// drain leaves the durable log at exactly the dangling
    /// `ActivityScheduled`/`ActivityStarted` a kill -9 would leave (the proven
    /// re-dispatchable state) while still unblocking the blocking dispatcher
    /// thread, so process exit is never wedged on tokio's blocking pool. A
    /// dispatch with no matching waiter (already resolved) is a no-op — a park
    /// must never be routed as an outbox failure delivery.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] when sink state cannot be trusted.
    fn park_activity(
        &self,
        workflow_id: &WorkflowId,
        activity_id: &ActivityId,
    ) -> Result<(), ServerError>;
}

/// Decode and hand a worker result to the engine-owned activity completion sink.
///
/// # Errors
///
/// Returns [`ServerError`] for malformed wire results or sink failures.
pub fn handle_activity_result(
    sink: &impl ActivityCompletionSink,
    result: ProtoActivityResult,
) -> Result<(), ServerError> {
    sink.complete_activity(ActivityCompletion::try_from(result)?)
}

/// Build the retryable failure reported when a worker loses ownership of an in-flight task.
///
/// The retryable classification models worker loss as infrastructure failure: aion-server
/// only reports the failure to the engine activity contract; the engine remains responsible
/// for applying the activity retry policy.
#[must_use]
pub fn lost_worker_error(worker_id: crate::worker::registry::WorkerId) -> ActivityError {
    ActivityError {
        kind: ActivityErrorKind::Retryable,
        message: format!("worker {worker_id:?} lost before reporting activity result"),
        details: None,
    }
}

fn wire_error(message: &'static str) -> ServerError {
    ServerError::Wire {
        wire: WireError::backend(message),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use aion_core::{ActivityErrorKind, ContentType};
    use aion_proto::{ProtoActivityError, ProtoActivityErrorKind};
    use serde_json::json;
    use uuid::Uuid;

    use crate::worker::registry::ConnectedWorkerRegistry;

    use super::*;

    fn workflow_id() -> WorkflowId {
        WorkflowId::new(Uuid::nil())
    }

    fn activity_id() -> ActivityId {
        ActivityId::from_sequence_position(42)
    }

    fn payload(value: &serde_json::Value) -> Result<Payload, Box<dyn std::error::Error>> {
        Ok(Payload::from_json(value)?)
    }

    #[tokio::test]
    async fn dispatch_pushes_activity_task_with_correlation()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = ConnectedWorkerRegistry::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let activity_types = [String::from("charge-card")];
        let registration = registry.register("tenant-a", activity_types.iter(), tx)?;
        let dispatcher = ActivityDispatcher::new(registry.clone());
        let input = payload(&json!({"amount": 1200}))?;
        let scheduled = ScheduledActivity {
            namespace: String::from("tenant-a"),
            task_queue: String::from("default"),
            activity_type: String::from("charge-card"),
            node: None,
            workflow_id: workflow_id(),
            activity_id: activity_id(),
            run_id: None,
            input: input.clone(),
            attempt: 1,
            labels: std::collections::BTreeMap::new(),
        };

        dispatcher.dispatch(&scheduled).await?;
        let message = rx.recv().await.ok_or("expected pushed activity task")?;
        let WorkerMessage::ActivityTask(task) = message else {
            return Err("expected activity task message".into());
        };

        assert_eq!(task.workflow_id, Some(ProtoWorkflowId::from(workflow_id())));
        assert_eq!(task.activity_id, Some(ProtoActivityId::from(activity_id())));
        assert_eq!(task.activity_type, "charge-card");
        assert_eq!(task.input, Some(ProtoPayload::from(input)));
        assert_eq!(task.attempt, 1, "wire task must carry the stamped attempt");

        registration.deregister()?;
        Ok(())
    }

    #[tokio::test]
    async fn dispatch_waits_for_worker_then_delivers() -> Result<(), Box<dyn std::error::Error>> {
        let registry = ConnectedWorkerRegistry::default();
        let dispatcher = ActivityDispatcher::new(registry.clone());
        let scheduled = ScheduledActivity {
            namespace: String::from("tenant-a"),
            task_queue: String::from("default"),
            activity_type: String::from("charge-card"),
            node: None,
            workflow_id: workflow_id(),
            activity_id: activity_id(),
            run_id: None,
            input: Payload::new(ContentType::Json, b"{}".to_vec()),
            attempt: 1,
            labels: std::collections::BTreeMap::new(),
        };

        let dispatch_handle = tokio::spawn({
            let dispatcher = dispatcher.clone();
            let scheduled = scheduled.clone();
            async move { dispatcher.dispatch(&scheduled).await }
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!dispatch_handle.is_finished(), "dispatch should be waiting");

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let activity_types = [String::from("charge-card")];
        let _registration = registry.register("tenant-a", activity_types.iter(), tx)?;

        dispatch_handle.await??;
        assert!(rx.recv().await.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn dispatch_skips_closed_worker_and_uses_next_match()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = ConnectedWorkerRegistry::default();
        let (closed_tx, closed_rx) = tokio::sync::mpsc::channel(1);
        let (live_tx, mut live_rx) = tokio::sync::mpsc::channel(1);
        let activity_types = [String::from("charge-card")];
        let closed_registration =
            registry.register("tenant-a", activity_types.iter(), closed_tx)?;
        let live_registration = registry.register("tenant-a", activity_types.iter(), live_tx)?;
        drop(closed_rx);

        let dispatcher = ActivityDispatcher::new(registry.clone());
        let scheduled = ScheduledActivity {
            namespace: String::from("tenant-a"),
            task_queue: String::from("default"),
            activity_type: String::from("charge-card"),
            node: None,
            workflow_id: workflow_id(),
            activity_id: activity_id(),
            run_id: None,
            input: Payload::new(ContentType::Json, b"{}".to_vec()),
            attempt: 1,
            labels: std::collections::BTreeMap::new(),
        };

        dispatcher.dispatch(&scheduled).await?;

        assert!(live_rx.recv().await.is_some());
        assert_eq!(
            registry
                .workers_for("tenant-a", "default", "charge-card", None)?
                .len(),
            1
        );

        closed_registration.deregister()?;
        live_registration.deregister()?;
        Ok(())
    }

    fn scheduled_unpinned() -> ScheduledActivity {
        ScheduledActivity {
            namespace: String::from("tenant-a"),
            task_queue: String::from("default"),
            activity_type: String::from("charge-card"),
            // UNPINNED row: `node == None`, so placement (here a Pinned require) is
            // the worker-selection input — the row's own node is never set.
            node: None,
            workflow_id: workflow_id(),
            activity_id: activity_id(),
            run_id: None,
            input: Payload::new(ContentType::Json, b"{}".to_vec()),
            attempt: 1,
            labels: std::collections::BTreeMap::new(),
        }
    }

    fn required(labels: &[&str]) -> std::collections::BTreeSet<String> {
        labels.iter().map(|l| (*l).to_owned()).collect()
    }

    /// P2-I1 gRPC hard-pin: an unpinned row in a `Pinned{n1}` namespace WAITS when
    /// no `n1` worker is live and NEVER spills to a live any-node worker — the
    /// opposite of `Prefer`. This test would FAIL under the old fall-through (which
    /// dispatched `Pinned` to any worker).
    #[tokio::test]
    async fn dispatch_requiring_waits_and_never_spills_to_a_wrong_node_worker()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = ConnectedWorkerRegistry::default();
        let dispatcher = ActivityDispatcher::new(registry.clone());
        let scheduled = scheduled_unpinned();
        let types = [String::from("charge-card")];

        // A LIVE worker on the WRONG node (n2) — a Prefer would spill to it; a
        // Pinned{n1} must NOT.
        let (wrong_tx, mut wrong_rx) = tokio::sync::mpsc::channel(1);
        let _wrong = registry.register_namespaces(
            [String::from("tenant-a")],
            "default",
            Some(String::from("n2")),
            types.iter(),
            wrong_tx,
        )?;

        let handle = tokio::spawn({
            let dispatcher = dispatcher.clone();
            let scheduled = scheduled.clone();
            async move {
                dispatcher
                    .dispatch_requiring(&scheduled, &required(&["n1"]))
                    .await
            }
        });

        // The wrong-node worker is idle and live, yet dispatch must still be waiting.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !handle.is_finished(),
            "Pinned{{n1}} must WAIT rather than spill to the live n2 worker"
        );
        assert!(
            wrong_rx.try_recv().is_err(),
            "the wrong-node (n2) worker must never receive the task"
        );

        // Bring up the REQUIRED n1 worker: the wait resolves onto it.
        let (right_tx, mut right_rx) = tokio::sync::mpsc::channel(1);
        let _right = registry.register_namespaces(
            [String::from("tenant-a")],
            "default",
            Some(String::from("n1")),
            types.iter(),
            right_tx,
        )?;

        handle.await??;
        assert!(
            right_rx.recv().await.is_some(),
            "the required n1 worker receives the task once live"
        );
        assert!(
            wrong_rx.try_recv().is_err(),
            "the wrong-node worker still never received it"
        );
        Ok(())
    }

    /// P2-I1 determinism: the row's authored `node` stays `None` through a Pinned
    /// dispatch — placement is a pure selection input, never written back.
    #[tokio::test]
    async fn dispatch_requiring_never_mutates_the_rows_node()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = ConnectedWorkerRegistry::default();
        let dispatcher = ActivityDispatcher::new(registry.clone());
        let scheduled = scheduled_unpinned();
        assert_eq!(scheduled.node, None, "precondition: the row is unpinned");
        let types = [String::from("charge-card")];
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let _right = registry.register_namespaces(
            [String::from("tenant-a")],
            "default",
            Some(String::from("n1")),
            types.iter(),
            tx,
        )?;

        dispatcher
            .dispatch_requiring(&scheduled, &required(&["n1"]))
            .await?;

        assert!(rx.recv().await.is_some(), "the n1 worker received the task");
        assert_eq!(
            scheduled.node, None,
            "the row's authored node MUST remain None through a Pinned dispatch \
             (the determinism invariant, CP-Phase-2 §2.4)"
        );
        Ok(())
    }

    #[derive(Default)]
    struct RecordingSink {
        completions: Mutex<Vec<ActivityCompletion>>,
    }

    impl ActivityCompletionSink for RecordingSink {
        fn complete_activity(&self, completion: ActivityCompletion) -> Result<(), ServerError> {
            self.completions
                .lock()
                .map_err(|_| ServerError::lock_poisoned("recording completion sink"))?
                .push(completion);
            Ok(())
        }

        fn park_activity(
            &self,
            _workflow_id: &WorkflowId,
            _activity_id: &ActivityId,
        ) -> Result<(), ServerError> {
            Err(ServerError::worker_dispatch(
                "",
                "",
                "result-handoff tests never park a dispatch",
            ))
        }
    }

    #[test]
    fn successful_activity_result_calls_completion_sink() -> Result<(), Box<dyn std::error::Error>>
    {
        let sink = RecordingSink::default();
        let output = payload(&json!({"ok": true}))?;
        let result = ProtoActivityResult {
            workflow_id: Some(ProtoWorkflowId::from(workflow_id())),
            activity_id: Some(ProtoActivityId::from(activity_id())),
            run_id: None,
            outcome: Some(proto_activity_result::Outcome::Result(ProtoPayload::from(
                output.clone(),
            ))),
        };

        handle_activity_result(&sink, result)?;
        let completions = sink
            .completions
            .lock()
            .map_err(|_| ServerError::lock_poisoned("recording completion sink"))?;

        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].workflow_id, workflow_id());
        assert_eq!(completions[0].activity_id, activity_id());
        assert_eq!(
            completions[0].outcome,
            ActivityCompletionOutcome::Succeeded(output)
        );
        Ok(())
    }

    #[test]
    fn failed_activity_result_preserves_error_classification()
    -> Result<(), Box<dyn std::error::Error>> {
        let sink = RecordingSink::default();
        let error = ProtoActivityError {
            kind: ProtoActivityErrorKind::Retryable as i32,
            message: String::from("temporary outage"),
            details: Some(ProtoPayload::from(payload(
                &json!({"retry_after_ms": 500}),
            )?)),
        };
        let result = ProtoActivityResult {
            workflow_id: Some(ProtoWorkflowId::from(workflow_id())),
            activity_id: Some(ProtoActivityId::from(activity_id())),
            run_id: None,
            outcome: Some(proto_activity_result::Outcome::Error(error)),
        };

        handle_activity_result(&sink, result)?;
        let completions = sink
            .completions
            .lock()
            .map_err(|_| ServerError::lock_poisoned("recording completion sink"))?;

        assert_eq!(completions.len(), 1);
        match &completions[0].outcome {
            ActivityCompletionOutcome::Failed(error) => {
                assert_eq!(error.kind, ActivityErrorKind::Retryable);
                assert!(error.is_retryable());
            }
            ActivityCompletionOutcome::Succeeded(_) => return Err("expected failed outcome".into()),
        }
        Ok(())
    }
}
