//! Per-engine state recovered by NIFs through beamr's NIF private data.
//!
//! Every engine instance installs exactly one [`EngineNifState`] on its
//! scheduler (`SchedulerConfig::nif_private_data`); every NIF call recovers
//! it from its [`ProcessContext`]. This replaces process-wide statics, which
//! cross-wired engines whenever two coexisted in one OS process: beamr pids
//! are per-scheduler (each instance numbers from 1), so a pid-keyed global
//! resolved workflows against whichever engine installed last.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, RwLock};

use aion_core::TimerId;
use beamr::native::ProcessContext;
use chrono::{DateTime, Utc};
use dashmap::DashMap;

use crate::activity::bridge::ActivityDispatcher;

use super::nif_activity::RuntimeContext;
use super::nif_child_engine::ChildNifBridge;
use super::nif_determinism::NifContextSource;
use super::nif_query::{QueryBridgeState, QueryHandlers};
use super::nif_signal::SignalNifBridge;
use super::nif_timeout::TimeoutScope;
use super::nif_timer::TimerNifBridge;

/// Engine-scoped state shared by every NIF of one engine instance.
///
/// Slots are filled during engine build (and by services that start after
/// the scheduler, such as the timer bridge); NIF calls read them through
/// [`engine_nif_state`]. All interior mutability is engine-local, so two
/// engines in one OS process are fully isolated.
#[derive(Default)]
pub(crate) struct EngineNifState {
    /// Registry/runtime/tokio seams used by activity and lifecycle NIFs.
    pub(super) runtime_context: RwLock<Option<RuntimeContext>>,
    /// Activity dispatcher executing activity work in-process.
    pub(super) activity_dispatcher: RwLock<Option<Arc<dyn ActivityDispatcher>>>,
    /// Context source for deterministic time/random NIFs.
    pub(super) context_source: RwLock<Option<Arc<NifContextSource>>>,
    /// Signal delivery bridge.
    pub(super) signal_bridge: RwLock<Option<Arc<SignalNifBridge>>>,
    /// Child workflow bridge.
    pub(super) child_bridge: RwLock<Option<Arc<ChildNifBridge>>>,
    /// Query bridge state (registry, engine handle, mailbox engine).
    pub(super) query_bridge: Mutex<Option<QueryBridgeState>>,
    /// Registered query handlers and pending query responses.
    pub(super) query_handlers: QueryHandlers,
    /// Timer bridge owning pending timer tasks and the delivery queue.
    pub(super) timer_bridge: Mutex<Option<Arc<TimerNifBridge>>>,
    /// Armed `with_timeout` deadline scopes by scope id.
    pub(super) timeout_scopes: DashMap<u64, TimeoutScope>,
    /// Scope-id stacks by workflow pid, innermost last.
    pub(super) timeout_scope_stacks: DashMap<u64, Vec<u64>>,
    /// Monotonic `with_timeout` scope id source.
    pub(super) next_timeout_scope_id: AtomicU64,
    /// Identity of the blocking await each suspended workflow pid is parked
    /// on, pinned at first live arrival so every re-entry of the suspended
    /// native resolves the same logical operation. A process runs one
    /// blocking await at a time; the entry is cleared on every terminal
    /// resolution and when the workflow process ends.
    pub(super) pending_awaits: DashMap<u64, PendingAwait>,
}

/// The await a suspended workflow process is parked on.
///
/// Stored in [`EngineNifState::pending_awaits`] keyed by pid. Pinning the
/// identity (timer id / signal occurrence) at first arrival keeps re-entries
/// deterministic: ordinal sequences advance on allocation, so a re-invoked
/// native must reuse the identity it allocated the first time, not draw a
/// fresh one.
#[derive(Clone, Debug)]
pub(crate) enum PendingAwait {
    /// `sleep` parked on an anonymous durable timer.
    Sleep {
        /// Deterministic anonymous timer identity allocated at first arrival.
        timer_id: TimerId,
        /// Absolute fire deadline recorded with the timer start.
        fire_at: DateTime<Utc>,
    },
    /// `receive_signal` parked on a named signal occurrence.
    Signal {
        /// Per-name zero-based occurrence index pinned at first arrival.
        index: usize,
    },
}

impl EngineNifState {
    /// Install the activity dispatcher executing this engine's activities.
    pub(crate) fn set_activity_dispatcher(&self, dispatcher: Arc<dyn ActivityDispatcher>) {
        match self.activity_dispatcher.write() {
            Ok(mut slot) => *slot = Some(dispatcher),
            Err(poisoned) => *poisoned.into_inner() = Some(dispatcher),
        }
    }

    /// Look up this engine's activity dispatcher.
    pub(crate) fn activity_dispatcher(&self) -> Option<Arc<dyn ActivityDispatcher>> {
        match self.activity_dispatcher.read() {
            Ok(slot) => slot.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Drop every per-process entry an exited workflow pid left behind.
    ///
    /// Called from the runtime process monitor when a workflow process ends
    /// (any outcome), so awaits and timeout scopes interrupted by the exit
    /// never leak map entries. beamr pids are never reused within a
    /// scheduler, so removal cannot race a new process under the same key.
    pub(crate) fn cleanup_process(&self, pid: u64) {
        self.pending_awaits.remove(&pid);
        self.timeout_scope_stacks.remove(&pid);
        self.timeout_scopes.retain(|_, scope| scope.pid != pid);
    }
}

/// Recover the calling engine's NIF state from a native-call context.
///
/// # Errors
///
/// Returns a human-readable reason when the runtime carries no private data
/// (the engine was not built through `EngineBuilder`) or the data has an
/// unexpected type.
pub(crate) fn engine_nif_state(ctx: &ProcessContext) -> Result<Arc<EngineNifState>, String> {
    let data = ctx
        .nif_private_data()
        .ok_or_else(|| "engine NIF state is not installed for this runtime".to_owned())?;
    Arc::clone(data)
        .downcast::<EngineNifState>()
        .map_err(|_| "engine NIF private data has an unexpected type".to_owned())
}
