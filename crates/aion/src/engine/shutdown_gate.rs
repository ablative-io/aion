//! Engine shutdown gating: lifecycle operations are counted in, shutdown
//! closes the gate and waits for the count to drain before tearing the
//! runtime down.

use std::sync::{Arc, Condvar, Mutex};

use crate::EngineError;

#[derive(Clone, Default)]
pub(super) struct ShutdownGate {
    inner: Arc<ShutdownGateInner>,
}

#[derive(Default)]
struct ShutdownGateInner {
    state: Mutex<ShutdownState>,
    idle: Condvar,
}

#[derive(Default)]
struct ShutdownState {
    shutting_down: bool,
    active_operations: usize,
}

impl ShutdownGate {
    pub(super) fn begin_start(&self) -> Result<LifecycleOperation, EngineError> {
        let mut state = self.state()?;
        if state.shutting_down {
            return Err(EngineError::ShuttingDown);
        }
        state.active_operations += 1;
        Ok(LifecycleOperation {
            inner: Arc::clone(&self.inner),
        })
    }

    pub(super) fn begin_operation(&self) -> Result<LifecycleOperation, EngineError> {
        let mut state = self.state()?;
        state.active_operations += 1;
        Ok(LifecycleOperation {
            inner: Arc::clone(&self.inner),
        })
    }

    pub(super) fn close_and_wait(&self) -> Result<(), EngineError> {
        let mut state = self.state()?;
        state.shutting_down = true;
        while state.active_operations > 0 {
            state = self
                .inner
                .idle
                .wait(state)
                .map_err(|_| EngineError::RegistryPoisoned)?;
        }
        Ok(())
    }

    fn state(&self) -> Result<std::sync::MutexGuard<'_, ShutdownState>, EngineError> {
        self.inner
            .state
            .lock()
            .map_err(|_| EngineError::RegistryPoisoned)
    }
}

pub(super) struct LifecycleOperation {
    inner: Arc<ShutdownGateInner>,
}

impl Drop for LifecycleOperation {
    fn drop(&mut self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.active_operations = state.active_operations.saturating_sub(1);
            if state.active_operations == 0 {
                self.inner.idle.notify_all();
            }
        }
    }
}
