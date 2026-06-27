//! Activity dispatch bridge for the `aion_flow_ffi` activity NIFs.
//!
//! The bridge decouples the raw NIF function pointer (which cannot capture
//! state) from the engine's activity execution path. A concrete dispatcher
//! is installed on the engine's NIF state during build; the NIF recovers it
//! from its calling context's NIF private data.

use std::collections::BTreeMap;
use std::sync::Arc;

use aion_core::{ActivityId, WorkflowId};
use futures::future::BoxFuture;

/// A fully-resolved activity dispatch crossing the engine→transport seam.
///
/// The engine builds this once the durability layer has assigned the
/// activity its ordinal, so it carries the *real* owning [`WorkflowId`] and
/// the *real* per-workflow [`ActivityId`] recorded in history — not a
/// transport-fabricated correlation token. A worker that logs these ids can
/// therefore be correlated directly against the event store, and a result
/// re-reported from a previous worker session keys to the exact execution it
/// belongs to.
///
/// `input` and `config` are the JSON strings the Gleam SDK sends through the
/// `aion_flow_ffi:dispatch_activity/3` binding; `attempt` is the one-based
/// delivery attempt the engine stamps onto the transport (the worker wire's
/// `ActivityTask.attempt`), so consumers distinguish retries without guessing.
#[derive(Clone, Debug)]
pub struct ActivityDispatch {
    /// Namespace selected for worker matching — the correctness/isolation
    /// boundary the activity may dispatch within.
    pub namespace: String,
    /// Task queue (pool/flavour) selected within the namespace. The worker-pool
    /// address is `(namespace, task_queue)`. There is no workflow-level
    /// selection yet, so the engine seam stamps the named `"default"` pool until
    /// the Gleam SDK selection (NSTQ-4) lands.
    pub task_queue: String,
    /// OPTIONAL node affinity (NODE-4): the concrete worker host this dispatch is
    /// pinned to within the `(namespace, task_queue)` pool. `None` = no affinity
    /// (any worker in the pool). Resolved once at the schedule seam from the
    /// SDK's per-activity `node` selection; there is no workflow-level default.
    pub node: Option<String>,
    /// Real owning workflow id, recorded in history at `WorkflowStarted`.
    pub workflow_id: WorkflowId,
    /// Real per-workflow activity ordinal, recorded at `ActivityScheduled`.
    pub activity_id: ActivityId,
    /// Registered activity-type name to match against worker registrations.
    pub name: String,
    /// JSON-encoded activity input.
    pub input: String,
    /// JSON-encoded dispatch config (retry/timeout/heartbeat policy).
    pub config: String,
    /// One-based delivery attempt for this dispatch.
    pub attempt: u32,
    /// Human-meaningful display labels the workflow attached to the activity
    /// (for example `brief=IP-001`, `repo=ablative-io/yggdrasil`). The engine
    /// never interprets these; they ride to the worker purely so its logs and
    /// the dashboard can show what a dispatch is working on. `BTreeMap` keeps
    /// the rendered order stable.
    pub labels: BTreeMap<String, String>,
}

/// Executes an activity request originating from workflow code.
///
/// The return value is the JSON-encoded activity result or a prefixed error
/// string matching the SDK's error-decoding convention.
pub trait ActivityDispatcher: Send + Sync + 'static {
    /// Dispatch the activity and block until completion.
    ///
    /// Returns `Ok(encoded_output)` on success or `Err(error_string)` on
    /// failure. Both sides are strings matching the Gleam SDK's
    /// `Result(String, String)` FFI contract.
    ///
    /// # Errors
    ///
    /// Returns the error string surfaced by the activity execution path —
    /// worker rejection, decode failure, timeout, or activity body error.
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String>;

    /// Dispatch the activity from a Tokio task.
    ///
    /// The default runs the synchronous [`Self::dispatch`] on the runtime's
    /// blocking pool via [`tokio::task::spawn_blocking`], so a dispatcher that
    /// blocks its calling thread cannot wedge the engine's async workers (a
    /// single-threaded engine runtime keeps servicing queries and completions
    /// while the dispatch waits). Nonblocking dispatchers can override this.
    ///
    /// Must be awaited inside a Tokio runtime context; the engine's
    /// completion task guarantees that.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::dispatch`], plus a dispatch-failure
    /// reason when the blocking task itself is cancelled or panics.
    fn dispatch_async(
        self: Arc<Self>,
        request: ActivityDispatch,
    ) -> BoxFuture<'static, Result<String, String>> {
        Box::pin(async move {
            let blocking = tokio::task::spawn_blocking(move || self.dispatch(request));
            match blocking.await {
                Ok(result) => result,
                Err(join_error) => Err(format!("activity dispatch task failed: {join_error}")),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use std::collections::BTreeMap;

    use aion_core::{ActivityId, WorkflowId};

    use super::{ActivityDispatch, ActivityDispatcher};
    use crate::runtime::EngineNifState;

    struct Echo;

    impl ActivityDispatcher for Echo {
        fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
            Ok(request.input)
        }
    }

    fn echo_request(input: &str) -> ActivityDispatch {
        ActivityDispatch {
            namespace: "default".to_owned(),
            task_queue: "default".to_owned(),
            node: None,
            workflow_id: WorkflowId::new_v4(),
            activity_id: ActivityId::from_sequence_position(0),
            name: "test".to_owned(),
            input: input.to_owned(),
            config: "{}".to_owned(),
            attempt: 1,
            labels: BTreeMap::new(),
        }
    }

    #[test]
    fn dispatcher_is_accessible_after_install_on_engine_state() {
        let state = EngineNifState::default();
        state.set_activity_dispatcher(Arc::new(Echo));
        let dispatcher = state.activity_dispatcher();
        assert!(dispatcher.is_some());
        assert_eq!(
            dispatcher
                .as_ref()
                .and_then(|d| d.dispatch(echo_request("hello")).ok()),
            Some("hello".to_owned())
        );
    }
}
