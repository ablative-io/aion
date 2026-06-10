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

use beamr::native::ProcessContext;
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
